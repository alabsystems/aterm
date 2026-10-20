// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// A crafted block's inflated `count` must not size the retained Vec: its spare
/// capacity escapes any len-based byte count.
#[test]
fn deserialize_lines_bounds_capacity_for_inflated_count() {
    // A well-formed single-line block: [count=1][valid line record].
    let one = serialize_lines(&[Line::from("hello")]);
    let line_record = &one[4..];

    let mut data = Vec::new();
    data.extend_from_slice(&u32::MAX.to_le_bytes()); // inflated count
    data.extend_from_slice(line_record); // one valid line
    // Break record: version=1, content_len=u32::MAX → line_size returns None
    // (attrs_start lands past the slice), so parsing stops right here.
    data.extend_from_slice(&[1u8, 0, 0xFF, 0xFF, 0xFF, 0xFF, 0]);
    // Pad so max_possible = (len-4)/5 exceeds the 16_384 cap (need > ~82 KiB).
    // The padding is never parsed (the break record halts the loop first).
    let target_len = 4 + line_record.len() + 7 + 90_000;
    data.resize(target_len, 0u8);

    let lines = deserialize_lines(&data);

    assert_eq!(lines.len(), 1, "only the one valid line parses");
    assert_eq!(lines[0].to_string(), "hello");
    assert!(
        lines.capacity() < 64,
        "capacity {} must track real content, not the inflated count",
        lines.capacity()
    );
}

// ============================================================================
// RLE Attribute Tests
// ============================================================================

#[test]
fn cell_attrs_from_raw_preserves_main_cell_wide_geometry() {
    const BOLD: u16 = 1 << 0;
    const WIDE: u16 = 1 << 9;
    const CONTINUATION_OR_PROTECTED: u16 = 1 << 10;
    // Once a style-interned cell's bit; a stored line's `OPENS_CELL`.
    const USES_STYLE_ID: u16 = 1 << 14;
    const COMPLEX: u16 = 1 << 15;

    let attrs = CellAttrs::from_raw(
        DEFAULT_FG,
        DEFAULT_BG,
        BOLD | WIDE | CONTINUATION_OR_PROTECTED | USES_STYLE_ID | COMPLEX,
    );
    assert_eq!(
        attrs.flags,
        BOLD | WIDE,
        "Line keeps the wide main cell's write-time geometry, but not contextual spacer/storage bits"
    );
    assert_eq!(
        CellAttrs::deserialize(&attrs.serialize()),
        Some(attrs),
        "the existing u16 wire field round-trips WIDE without a format change"
    );
    let opens = attrs.with_opens_cell(true);
    assert!(opens.opens_cell() && !attrs.opens_cell());
    assert_eq!(
        CellAttrs::deserialize(&opens.serialize()),
        Some(opens),
        "the wire field round-trips OPENS_CELL"
    );
    assert_eq!(opens.with_opens_cell(false), attrs);
}

/// The run-cursor matches `get_attr` at every index (monotone walks, repeats,
/// past-the-end reads, the backward rewind) over many-run and no-attr lines.
#[test]
fn attr_cursor_matches_get_attr_everywhere() {
    // 7 runs of varying length, alternating styles.
    let mut rle: Rle<CellAttrs> = Rle::new();
    let styles = [
        CellAttrs::new(0x01_FF0000, DEFAULT_BG, 0),
        CellAttrs::DEFAULT,
        CellAttrs::new(0x01_00FF00, DEFAULT_BG, 1),
    ];
    let lens = [3usize, 1, 4, 2, 5, 1, 4];
    for (i, &len) in lens.iter().enumerate() {
        for _ in 0..len {
            rle.push(styles[i % styles.len()]);
        }
    }
    let total: usize = lens.iter().sum();
    let line = Line::with_attrs(&"x".repeat(total), rle);

    // Monotone walk incl. repeats and past-the-end.
    let mut cursor = line.attr_cursor();
    for idx in [0usize, 0, 1, 2, 3, 3, 7, 8, 9, 12, 15, 19, 20, 25, 100] {
        assert_eq!(
            cursor.attr_at(idx),
            line.get_attr(idx),
            "monotone cursor diverged at {idx}"
        );
    }
    // Backward rewind stays correct.
    assert_eq!(cursor.attr_at(2), line.get_attr(2), "rewind to 2");
    assert_eq!(cursor.attr_at(11), line.get_attr(11), "re-advance to 11");

    // A line with no attrs: DEFAULT everywhere, no panic.
    let plain = Line::from("plain");
    let mut cursor = plain.attr_cursor();
    for idx in [0usize, 3, 40] {
        assert_eq!(cursor.attr_at(idx), CellAttrs::DEFAULT);
    }
}

#[test]
fn line_with_attrs_all_default() {
    let mut rle: Rle<CellAttrs> = Rle::new();
    for _ in 0..10 {
        rle.push(CellAttrs::DEFAULT);
    }

    // When all attrs are default, the optimization should drop them
    let line = Line::with_attrs("HelloWorld", rle);
    assert!(!line.has_attrs());
    assert_eq!(line.attr_run_count(), 0);
}

#[test]
fn line_serialize_roundtrip_with_attrs() {
    let mut rle: Rle<CellAttrs> = Rle::new();
    let red = CellAttrs::new(0x01_FF0000, DEFAULT_BG, 0);
    let green = CellAttrs::new(0x01_00FF00, DEFAULT_BG, 0);
    for _ in 0..3 {
        rle.push(red);
    }
    for _ in 0..7 {
        rle.push(green);
    }

    let mut line = Line::with_attrs("HelloWorld", rle);
    line.set_wrapped(true);

    let serialized = line.serialize();
    let deserialized = Line::deserialize(&serialized).unwrap();

    assert_eq!(deserialized.to_string(), "HelloWorld");
    assert!(deserialized.is_wrapped());
    assert!(deserialized.has_attrs());
    assert_eq!(deserialized.attr_run_count(), 2);

    // Verify attrs
    assert_eq!(deserialized.get_attr(0).fg, 0x01_FF0000);
    assert_eq!(deserialized.get_attr(5).fg, 0x01_00FF00);
}

#[test]
fn serialize_lines_roundtrip_with_attrs() {
    let mut lines = Vec::new();

    // Line 0: plain text (no attrs)
    lines.push(Line::from("Plain text"));

    // Line 1: with red attrs
    let mut rle: Rle<CellAttrs> = Rle::new();
    let red = CellAttrs::new(0x01_FF0000, DEFAULT_BG, 0);
    for _ in 0..10 {
        rle.push(red);
    }
    lines.push(Line::with_attrs("Red styled", rle));

    // Line 2: with mixed attrs
    let mut rle2: Rle<CellAttrs> = Rle::new();
    for _ in 0..5 {
        rle2.push(CellAttrs::DEFAULT);
    }
    for _ in 0..5 {
        rle2.push(CellAttrs::new(0x01_0000FF, DEFAULT_BG, 0x01)); // blue, bold
    }
    lines.push(Line::with_attrs("Mixed text", rle2));

    let serialized = serialize_lines(&lines);
    let deserialized = deserialize_lines(&serialized);

    assert_eq!(deserialized.len(), 3);
    assert_eq!(deserialized[0].to_string(), "Plain text");
    assert!(!deserialized[0].has_attrs());

    assert_eq!(deserialized[1].to_string(), "Red styled");
    assert!(deserialized[1].has_attrs());
    assert_eq!(deserialized[1].get_attr(0).fg, 0x01_FF0000);

    assert_eq!(deserialized[2].to_string(), "Mixed text");
    assert!(deserialized[2].has_attrs());
    assert!(deserialized[2].get_attr(0).is_default());
    assert_eq!(deserialized[2].get_attr(5).fg, 0x01_0000FF);
    assert_eq!(deserialized[2].get_attr(5).flags, 0x01); // bold
}

// ============================================================================
// Hyperlink Tests
// ============================================================================

#[test]
fn line_with_multiple_hyperlinks() {
    let rle: Rle<CellAttrs> = Rle::new();
    let hyperlinks = vec![
        HyperlinkSpan::new(0, 5, Arc::from("https://first.com")),
        HyperlinkSpan::new(10, 20, Arc::from("https://second.com")),
    ];
    let line = Line::with_hyperlinks("First and Second links", rle, hyperlinks);

    assert!(line.has_hyperlinks());
    assert_eq!(line.hyperlink_count(), 2);
    assert_eq!(
        line.get_hyperlink(0).map(|s| s.as_ref()),
        Some("https://first.com")
    );
    assert!(line.get_hyperlink(6).is_none());
    assert_eq!(
        line.get_hyperlink(15).map(|s| s.as_ref()),
        Some("https://second.com")
    );
}

#[test]
fn deserialize_caps_oversized_hyperlink_url_and_id() {
    // A crafted checkpoint or cold page can declare a span whose url_len/id_len
    // exceed the OSC 8 ingestion caps: deserialize skips the over-cap URL span
    // and drops the over-cap id.
    use crate::line::line_codec::{MAX_HYPERLINK_ID_BYTES, MAX_HYPERLINK_URL_BYTES};

    let url_big: String = "h".repeat(MAX_HYPERLINK_URL_BYTES + 100);
    let id_big: String = "i".repeat(MAX_HYPERLINK_ID_BYTES + 50);
    let rle: Rle<CellAttrs> = Rle::new();
    let hyperlinks = vec![
        // (0..3) oversized URL -> the whole span is dropped on deserialize.
        HyperlinkSpan::new(0, 3, Arc::from(url_big.as_str())),
        // (3..6) valid URL, oversized id -> URL survives, id is dropped.
        HyperlinkSpan::with_id(
            3,
            6,
            Arc::from("https://ok.example"),
            Some(Arc::from(id_big.as_str())),
        ),
        // (6..9) fully valid -> survives intact (URL + id).
        HyperlinkSpan::with_id(
            6,
            9,
            Arc::from("https://fine.example"),
            Some(Arc::from("shortid")),
        ),
    ];
    let line = Line::with_hyperlinks("abcdefghi", rle, hyperlinks);

    // The serializer writes the true (oversized) lengths; the deserializer enforces
    // the caps. Roundtrip and assert the caps engaged.
    let serialized = line.serialize();
    let de =
        Line::deserialize(&serialized).expect("line with a capped hyperlink still deserializes");

    assert!(
        de.get_hyperlink(0).is_none(),
        "an over-cap hyperlink URL span must be skipped, not stored"
    );
    assert_eq!(
        de.get_hyperlink(3).map(|s| s.as_ref()),
        Some("https://ok.example"),
        "a valid URL alongside an over-cap id must survive"
    );
    assert!(
        de.get_hyperlink_span(3)
            .and_then(|s| s.id.as_ref())
            .is_none(),
        "an over-cap OSC 8 id must be dropped (URL kept)"
    );
    assert_eq!(
        de.get_hyperlink(6).map(|s| s.as_ref()),
        Some("https://fine.example"),
        "a fully valid span survives the cap check intact"
    );
    assert_eq!(
        de.get_hyperlink_span(6)
            .and_then(|s| s.id.as_ref())
            .map(|s| s.as_ref()),
        Some("shortid"),
        "a valid id survives intact"
    );
}

#[test]
fn deserialize_caps_hyperlink_span_count() {
    // A crafted line can declare u16::MAX hyperlink spans; deserialize caps the
    // rebuilt count at MAX_HYPERLINK_SPANS (see its doc).
    use crate::line::line_codec::MAX_HYPERLINK_SPANS;

    // 1-column spans at distinct columns, well over the cap.
    let n = MAX_HYPERLINK_SPANS + 500;
    let mut hyperlinks = Vec::with_capacity(n);
    for i in 0..n {
        let c = (i % 60000) as u16;
        hyperlinks.push(HyperlinkSpan::new(
            c,
            c.saturating_add(1),
            Arc::from("https://x"),
        ));
    }
    let rle: Rle<CellAttrs> = Rle::new();
    let line = Line::with_hyperlinks("hi", rle, hyperlinks);

    let serialized = line.serialize();
    let de = Line::deserialize(&serialized).expect("a many-span line still deserializes");
    assert!(
        de.hyperlink_count() <= MAX_HYPERLINK_SPANS,
        "deserialized span count {} must be capped at MAX_HYPERLINK_SPANS ({MAX_HYPERLINK_SPANS})",
        de.hyperlink_count()
    );
    assert!(
        de.hyperlink_count() >= 1,
        "the cap keeps spans up to the limit, it does not drop them all"
    );
}

#[test]
fn line_underline_color_serialize_roundtrip() {
    // A line with SGR 58 underline colours serializes as v4 and round-trips the
    // packed colours (RGB 0x01.. and indexed 0x02..) by column, with the gap
    // between spans staying uncoloured.
    let rle: Rle<CellAttrs> = Rle::new();
    let mut line = Line::with_hyperlinks("underlined", rle, Vec::new());
    line.set_underline_colors(vec![
        UnderlineColorSpan::new(0, 4, 0x01_FF_00_00), // RGB red, cols 0..4
        UnderlineColorSpan::new(6, 10, 0x02_00_00_05), // indexed 5, cols 6..10
    ]);

    let serialized = line.serialize();
    assert_eq!(
        serialized[0], 4,
        "underline colours bump the line to version 4"
    );

    let deserialized = Line::deserialize(&serialized).unwrap();
    assert_eq!(deserialized.to_string(), "underlined");
    assert!(deserialized.has_underline_colors());
    assert_eq!(deserialized.underline_color_count(), 2);
    assert_eq!(deserialized.get_underline_color(0), Some(0x01_FF_00_00));
    assert_eq!(deserialized.get_underline_color(3), Some(0x01_FF_00_00));
    assert_eq!(
        deserialized.get_underline_color(4),
        None,
        "gap stays uncoloured"
    );
    assert_eq!(deserialized.get_underline_color(7), Some(0x02_00_00_05));
    assert_eq!(
        deserialized.get_underline_color(10),
        None,
        "past the last span"
    );
}

#[test]
fn line_without_underline_colors_stays_v3() {
    // No SGR 58: version 3 and no trailing section, so a v3 reader is unaffected.
    let rle: Rle<CellAttrs> = Rle::new();
    let hyperlinks = vec![HyperlinkSpan::new(0, 4, Arc::from("https://x.test"))];
    let line = Line::with_hyperlinks("hello", rle, hyperlinks);
    let serialized = line.serialize();
    assert_eq!(serialized[0], 3, "no underline colours → stays version 3");

    let deserialized = Line::deserialize(&serialized).unwrap();
    assert!(!deserialized.has_underline_colors());
    assert_eq!(deserialized.get_underline_color(0), None);
    assert!(deserialized.has_hyperlinks());
}

#[test]
fn serialize_lines_underline_colors_block_framing() {
    // A v4 line between two others: `line_size_v1v2` must include the underline
    // section, or every following record misframes.
    let mut lines = Vec::new();
    lines.push(Line::from("before"));

    let rle: Rle<CellAttrs> = Rle::new();
    let hyperlinks = vec![HyperlinkSpan::new(0, 3, Arc::from("https://mid.test"))];
    let mut mid = Line::with_hyperlinks("MIDDLE", rle, hyperlinks);
    mid.set_underline_colors(vec![
        UnderlineColorSpan::new(0, 3, 0x01_AA_BB_CC),
        UnderlineColorSpan::new(4, 6, 0x02_00_00_07),
    ]);
    lines.push(mid);

    lines.push(Line::from("after"));

    let serialized = serialize_lines(&lines);
    let d = deserialize_lines(&serialized);

    assert_eq!(
        d.len(),
        3,
        "all three records must reframe correctly past the v4 section"
    );
    assert_eq!(d[0].to_string(), "before");
    assert_eq!(d[1].to_string(), "MIDDLE");
    assert_eq!(
        d[2].to_string(),
        "after",
        "the record AFTER a v4 line must not be misframed"
    );
    // The middle line's underline colours and hyperlink survive the block roundtrip.
    assert_eq!(d[1].get_underline_color(1), Some(0x01_AA_BB_CC));
    assert_eq!(d[1].get_underline_color(3), None);
    assert_eq!(d[1].get_underline_color(5), Some(0x02_00_00_07));
    assert_eq!(
        d[1].get_hyperlink(1).map(|s| s.as_ref()),
        Some("https://mid.test")
    );
}

#[test]
fn line_underline_colors_no_hyperlinks_roundtrip() {
    // Underline colours with no hyperlinks: the section begins right after the
    // 2-byte hyperlink count (deserialize_hyperlinks_v3 must report that offset,
    // not `data.len()`, or the underline section would be missed).
    let rle: Rle<CellAttrs> = Rle::new();
    let mut line = Line::with_hyperlinks("only-ul", rle, Vec::new());
    line.set_underline_colors(vec![UnderlineColorSpan::new(0, 7, 0x02_00_00_09)]);
    let serialized = line.serialize();
    assert_eq!(serialized[0], 4);
    let d = Line::deserialize(&serialized).unwrap();
    assert!(
        d.has_underline_colors(),
        "underline colours survive with no hyperlinks"
    );
    assert!(!d.has_hyperlinks());
    assert_eq!(d.get_underline_color(3), Some(0x02_00_00_09));
}

#[test]
fn serialize_lines_with_hyperlinks_roundtrip() {
    let mut lines = Vec::new();

    // Line 0: plain text (no hyperlinks)
    lines.push(Line::from("Plain text"));

    // Line 1: with hyperlink
    let rle1: Rle<CellAttrs> = Rle::new();
    let hyperlinks1 = vec![HyperlinkSpan::new(0, 4, Arc::from("https://link.test"))];
    lines.push(Line::with_hyperlinks("Link here", rle1, hyperlinks1));

    // Line 2: with attrs and hyperlink
    let mut rle2: Rle<CellAttrs> = Rle::new();
    let red = CellAttrs::new(0x01_FF0000, DEFAULT_BG, 0);
    for _ in 0..10 {
        rle2.push(red);
    }
    let hyperlinks2 = vec![HyperlinkSpan::new(0, 10, Arc::from("https://styled.link"))];
    lines.push(Line::with_hyperlinks("Styled lnk", rle2, hyperlinks2));

    let serialized = serialize_lines(&lines);
    let deserialized = deserialize_lines(&serialized);

    assert_eq!(deserialized.len(), 3);

    assert!(!deserialized[0].has_hyperlinks());
    assert!(deserialized[1].has_hyperlinks());
    assert_eq!(
        deserialized[1].get_hyperlink(2).map(|s| s.as_ref()),
        Some("https://link.test")
    );

    assert!(deserialized[2].has_hyperlinks());
    assert!(deserialized[2].has_attrs());
    assert_eq!(
        deserialized[2].get_hyperlink(5).map(|s| s.as_ref()),
        Some("https://styled.link")
    );
    assert_eq!(deserialized[2].get_attr(0).fg, 0x01_FF0000);
}

#[test]
fn line_hyperlink_with_id_serialize_roundtrip() {
    let rle: Rle<CellAttrs> = Rle::new();
    let hyperlinks = vec![
        HyperlinkSpan::with_id(
            0,
            10,
            Arc::from("https://example.com"),
            Some(Arc::from("link-42")),
        ),
        HyperlinkSpan::with_id(15, 25, Arc::from("https://other.com"), None),
    ];
    let line = Line::with_hyperlinks("Click here or there!", rle, hyperlinks);

    let serialized = line.serialize();
    let deserialized = Line::deserialize(&serialized).unwrap();

    assert_eq!(deserialized.to_string(), "Click here or there!");
    assert!(deserialized.has_hyperlinks());
    assert_eq!(deserialized.hyperlink_count(), 2);

    // First span: has explicit ID
    let span0 = deserialized.get_hyperlink_span(5).unwrap();
    assert_eq!(span0.url.as_ref(), "https://example.com");
    assert_eq!(span0.id.as_deref(), Some("link-42"));

    // Second span: no ID
    let span1 = deserialized.get_hyperlink_span(20).unwrap();
    assert_eq!(span1.url.as_ref(), "https://other.com");
    assert!(span1.id.is_none());
}

// =============================================================================
// Robustness tests for untrusted input
// =============================================================================

/// A record declaring content past `MAX_LINE_CONTENT_BYTES` is rejected, not
/// materialized; a normal line round-trips.
#[test]
fn deserialize_rejects_oversized_line_content() {
    use crate::line::line_codec::MAX_LINE_CONTENT_BYTES;

    // Legitimate line round-trips unchanged.
    let ok = Line::from("hello");
    let ser_ok = ok.serialize();
    assert!(
        Line::deserialize(&ser_ok).is_some(),
        "a normal line deserializes"
    );

    // Forge a v1+ record header claiming content_len = cap + 1 (bytes need not
    // actually be present — the cap check fires before the length/bounds check).
    let bad_len = (MAX_LINE_CONTENT_BYTES + 1) as u32;
    let mut forged = vec![1u8, 0u8]; // version=1, flags=0
    forged.extend_from_slice(&bad_len.to_le_bytes());
    forged.extend_from_slice(&[b'x'; 8]); // some content bytes (not enough; irrelevant)
    assert!(
        Line::deserialize(&forged).is_none(),
        "a line claiming > MAX_LINE_CONTENT_BYTES of content is rejected"
    );
}

/// `deserialize_lines_capped` bounds the reconstructed line count, so a block of
/// many tiny records cannot amplify into a huge `Vec<Line>`; the uncapped
/// `deserialize_lines` (checkpoint path) still returns them all.
#[test]
fn deserialize_lines_capped_bounds_reconstructed_count() {
    // 1000 empty legacy (v0) records: [flags=0][len=0].
    let n = 1000usize;
    let mut data = (n as u32).to_le_bytes().to_vec();
    for _ in 0..n {
        data.push(0u8); // v0 flags (version byte 0 selects legacy)
        data.extend_from_slice(&0u32.to_le_bytes()); // content_len = 0
    }
    // Uncapped: all 1000 reconstructed.
    assert_eq!(
        deserialize_lines(&data).len(),
        n,
        "uncapped keeps every record"
    );
    // Capped at 10: only 10 reconstructed, the rest ignored (no amplification).
    assert_eq!(
        crate::line::line_codec_block::deserialize_lines_capped(&data, 10).len(),
        10,
        "the cap bounds the reconstructed line count"
    );
}

#[test]
fn strict_checkpoint_decoder_rejects_excess_rows_before_reconstruction() {
    let mut data = Vec::new();
    data.extend_from_slice(&2u32.to_le_bytes());
    for _ in 0..2 {
        data.push(0); // legacy record flags/version
        data.extend_from_slice(&0u32.to_le_bytes());
    }
    assert!(
        crate::line::line_codec_block::deserialize_lines_strict(
            &data,
            1,
            80,
            80 * 256,
            80 * 512 + 16 * 1024,
        )
        .is_none(),
        "declared rows+1 is rejected without decoding a line"
    );
}

#[test]
fn strict_checkpoint_decoder_rejects_attr_run_beyond_columns() {
    let mut record = vec![1, 0]; // v1, flags
    record.extend_from_slice(&1u32.to_le_bytes());
    record.push(b'x');
    record.push(1); // has attrs
    record.extend_from_slice(&1u32.to_le_bytes());
    record.extend_from_slice(&[0u8; 10]); // default CellAttrs wire
    record.extend_from_slice(&u32::MAX.to_le_bytes()); // hostile compressed run
    let mut data = Vec::new();
    data.extend_from_slice(&1u32.to_le_bytes());
    data.extend_from_slice(&record);
    assert!(
        crate::line::line_codec_block::deserialize_lines_strict(
            &data,
            1,
            80,
            80 * 256,
            80 * 512 + 16 * 1024,
        )
        .is_none(),
        "a compact RLE run cannot claim more attributes than the line has \
         characters AND more than meta.cols cells — the one-char line bounds it"
    );
}

/// The per-line cap the seamless-update consumer passes for a `cols`-wide grid
/// (`checkpoint_grid_is_canonical` in aterm-gui): `cols` cells, one 256-byte
/// grapheme unit per cell, and the record framing budget.
fn strict_decode_one_row(line: &Line, cols: usize) -> Option<Vec<Line>> {
    let data = serialize_lines(std::slice::from_ref(line));
    crate::line::line_codec_block::deserialize_lines_strict(
        &data,
        1,
        cols,
        cols * 256,
        16 * 1024 + cols * 512,
    )
}

/// A styled row whose attrs RLE has one entry per character, as the grid's
/// row→line conversion emits it (`push_cell_text` charges `chars().count()`,
/// one attr per combining mark, nothing for a wide spacer).
fn styled_row(text: &str) -> Line {
    let bold = CellAttrs::new(DEFAULT_FG, DEFAULT_BG, 1);
    let mut rle: Rle<CellAttrs> = Rle::new();
    for _ in text.chars() {
        rle.push(bold);
    }
    Line::with_attrs(text, rle)
}

/// A bold 149-column row of 148 `a` and an NFD `e` + U+0301 is 149 cells but
/// 150 characters; comparing the attrs count to the cell cap would refuse it.
#[test]
fn strict_checkpoint_decoder_admits_a_full_styled_row_with_a_combining_mark() {
    let text = format!("{}e\u{301}", "a".repeat(148));
    assert_eq!(text.chars().count(), 150, "150 characters in 149 cells");
    let line = styled_row(&text);
    let decoded = strict_decode_one_row(&line, 149)
        .expect("a combining mark on a full styled row is canonical content");
    assert_eq!(decoded.len(), 1);
    assert_eq!(
        serialize_lines(&decoded),
        serialize_lines(std::slice::from_ref(&line)),
        "the strict decode is lossless"
    );
}

/// The ZWJ variant: `👨‍💻` is three codepoints in two cells, so 147 `a` plus the
/// cluster fill 149 columns with 150 characters.
#[test]
fn strict_checkpoint_decoder_admits_a_full_styled_row_with_a_zwj_emoji() {
    let text = format!("{}\u{1F468}\u{200D}\u{1F4BB}", "a".repeat(147));
    assert_eq!(text.chars().count(), 150, "150 characters in 149 cells");
    let line = styled_row(&text);
    let decoded = strict_decode_one_row(&line, 149)
        .expect("a ZWJ cluster on a full styled row is canonical content");
    assert_eq!(
        serialize_lines(&decoded),
        serialize_lines(std::slice::from_ref(&line))
    );
}

/// The leniency is bounded by the line's own character count: an attrs RLE
/// that claims more entries than the line has characters and more than the
/// grid has columns is still refused.
#[test]
fn strict_checkpoint_decoder_still_refuses_attrs_beyond_both_bounds() {
    let bold = CellAttrs::new(DEFAULT_FG, DEFAULT_BG, 1);
    let mut rle: Rle<CellAttrs> = Rle::new();
    for _ in 0..81 {
        rle.push(bold);
    }
    // 10 characters, 81 attrs, an 80-column grid: over both bounds.
    let line = Line::with_attrs("0123456789", rle);
    assert!(
        strict_decode_one_row(&line, 80).is_none(),
        "attrs beyond both the column cap and the character count are hostile"
    );
    // Exactly `cols` attrs is admitted.
    let mut rle: Rle<CellAttrs> = Rle::new();
    for _ in 0..80 {
        rle.push(bold);
    }
    let line = Line::with_attrs("0123456789", rle);
    assert!(strict_decode_one_row(&line, 80).is_some());
}

/// Hyperlink and underline-colour spans are physical columns, not characters,
/// so the character-count leniency does not reach them: a span ending past
/// `cols` is refused even on a line with more characters than columns.
#[test]
fn strict_checkpoint_decoder_keeps_span_columns_bounded_by_cols() {
    let text = format!("{}e\u{301}", "a".repeat(148));
    let bold = CellAttrs::new(DEFAULT_FG, DEFAULT_BG, 1);
    let mut rle: Rle<CellAttrs> = Rle::new();
    for _ in text.chars() {
        rle.push(bold);
    }
    let inside = Line::with_hyperlinks(
        &text,
        rle.clone(),
        vec![HyperlinkSpan::new(0, 149, Arc::from("https://example.com"))],
    );
    assert!(
        strict_decode_one_row(&inside, 149).is_some(),
        "a span whose exclusive end is `cols` covers the last column and is in bounds"
    );
    let beyond = Line::with_hyperlinks(
        &text,
        rle,
        vec![HyperlinkSpan::new(0, 150, Arc::from("https://example.com"))],
    );
    assert!(
        strict_decode_one_row(&beyond, 149).is_none(),
        "a span cannot end past the grid's last column, whatever the character count"
    );
}

/// A 41-byte fuzz crash artifact: no decode path panics or overflows on it.
#[test]
fn fuzz_crash_4950_line_codec_no_overflow() {
    let data: &[u8] = &[
        0x31, 0x00, 0x00, 0x00, 0x00, 0x00, 0x55, 0x05, 0x00, 0x00, 0x5d, 0x00, 0x00, 0x63, 0x00,
        0x00, 0x00, 0x03, 0x00, 0x05, 0x3f, 0x00, 0x05, 0x05, 0x05, 0x00, 0x00, 0x00, 0x00, 0x5d,
        0xff, 0x7f, 0xff, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xfc,
    ];

    // Phase 1: deserialize_lines should produce no lines from this corrupt data.
    // The first v0 record's content_len (349440) exceeds the 37-byte payload,
    // so the block parser breaks immediately.
    let lines = deserialize_lines(data);
    assert!(
        lines.is_empty(),
        "corrupt fuzz data should produce no parseable lines, got {}",
        lines.len()
    );

    // Phase 2: Line::deserialize on full data — version 0x31 is treated as
    // non-legacy (any non-zero version uses v1+ format). Bytes 2..6 are zero
    // so content_len=0 and it parses as an empty-content line.
    let single = Line::deserialize(data);
    assert!(
        single.is_some(),
        "full data parses (version 0x31 treated as v1+)"
    );
    assert!(
        single.unwrap().to_string().is_empty(),
        "content should be empty (content_len=0 from zero bytes 2..6)"
    );
    // Sub-slices shorter than 7 bytes always return None (minimum for v1+).
    for split in 1..7 {
        assert!(
            Line::deserialize(&data[..split]).is_none(),
            "sub-slice of length {split} too short for v1+ parsing"
        );
    }
}

/// Crafted input: large content_len + large RLE run lengths to trigger
/// overflow in block-level size computation or RLE accumulation.
#[test]
fn fuzz_regression_large_content_len_no_overflow() {
    // Version 3 line with content_len = u32::MAX (0xFFFFFFFF)
    let mut data = vec![0u8; 12];
    data[0] = 0x01; // count = 1
    data[4] = 3; // version 3
    data[5] = 0; // flags
    data[6..10].copy_from_slice(&u32::MAX.to_le_bytes()); // content_len = u32::MAX

    // Phase 1: deserialize_lines should gracefully handle huge content_len
    let lines = deserialize_lines(&data);
    assert!(lines.is_empty(), "truncated data should produce no lines");

    // Phase 2: Line::deserialize should return None — content_len = u32::MAX
    // means the data is far too short to contain the declared content.
    assert!(
        Line::deserialize(&data[4..]).is_none(),
        "line with content_len=u32::MAX and only 8 bytes of data should not parse"
    );
}

/// Crafted: two RLE runs with lengths that sum to > u32::MAX.
/// Exercises saturating arithmetic in RLE extend_with.
#[test]
fn fuzz_regression_rle_run_length_overflow() {
    // Build a minimal v1 line with two RLE runs:
    //   run 0: length = 0x80000000 (2^31)
    //   run 1: length = 0x80000000 (2^31)
    // Sum = 2^32 which overflows u32. Rle::extend_with should saturate.
    let content = b"AB";
    let content_len = content.len() as u32;

    let mut wire = Vec::new();
    wire.push(1u8); // version
    wire.push(0u8); // flags
    wire.extend_from_slice(&content_len.to_le_bytes());
    wire.extend_from_slice(content);
    wire.push(1u8); // has_attrs = true
    wire.extend_from_slice(&2u32.to_le_bytes()); // run_count = 2

    // Run 0: 10 bytes attrs + 4 bytes length
    let attrs_bytes = CellAttrs::DEFAULT.serialize();
    wire.extend_from_slice(&attrs_bytes);
    wire.extend_from_slice(&0x8000_0000u32.to_le_bytes());

    // Run 1: different attrs so they don't merge
    let red = CellAttrs::new(0x01_FF0000, DEFAULT_BG, 0);
    wire.extend_from_slice(&red.serialize());
    wire.extend_from_slice(&0x8000_0000u32.to_le_bytes());

    // hyperlink count = 0
    wire.extend_from_slice(&0u16.to_le_bytes());

    // This should not panic — RLE should saturate
    let line = Line::deserialize(&wire);
    assert!(line.is_some(), "valid wire format should deserialize");
    let line = line.unwrap();
    assert_eq!(line.to_string(), "AB");
    assert!(line.has_attrs());
}

/// Colored output, OSC 8 links and SGR 58 all contribute to the block's
/// reservation; a content-only estimate grows and retains excess capacity.
#[test]
fn serialize_lines_capacity_covers_styled_hyperlinked_rows() {
    let mut lines = Vec::new();
    for _ in 0..64 {
        let mut attrs = Rle::new();
        for col in 0..48 {
            attrs.push(CellAttrs::new(0x01_000000 + col, DEFAULT_BG, 0));
        }
        let mut line = Line::with_hyperlinks_owned(
            "x".repeat(48),
            attrs,
            vec![HyperlinkSpan::with_id(
                0,
                48,
                Arc::from("https://example.test/build/output"),
                Some(Arc::from("build-result")),
            )],
        );
        line.set_underline_colors(vec![UnderlineColorSpan::new(0, 48, 0x01_AA_BB_CC)]);
        lines.push(line);
    }

    let serialized = serialize_lines(&lines);
    assert_eq!(serialized.capacity(), serialized.len());
    let roundtrip = deserialize_lines(&serialized);
    assert_eq!(roundtrip.len(), lines.len());
    for (actual, expected) in roundtrip.iter().zip(&lines) {
        assert_eq!(actual.serialize(), expected.serialize());
    }
}

/// serialize_lines handles empty and single-line blocks.
#[test]
fn serialize_lines_empty_and_single() {
    // Empty block.
    let serialized = serialize_lines(&[]);
    assert_eq!(serialized.len(), 4); // just the count header
    let deserialized = deserialize_lines(&serialized);
    assert!(deserialized.is_empty());

    // Single line.
    let lines = vec![Line::from("hello")];
    let serialized = serialize_lines(&lines);
    let deserialized = deserialize_lines(&serialized);
    assert_eq!(deserialized.len(), 1);
    assert_eq!(deserialized[0].to_string(), "hello");
}

#[test]
fn fuzz_serialize_deserialize_roundtrip() {
    // Any sequence of text lines must survive serialize → deserialize unchanged.
    let mut state: u64 = 0xBB67_AE85_84CA_A73B;
    let mut next = move || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (state >> 33) as u32
    };
    for _ in 0..10_000 {
        let n = (next() % 8) as usize;
        let lines: Vec<Line> = (0..n)
            .map(|_| {
                let wlen = (next() % 24) as usize;
                let s: String = (0..wlen)
                    .map(|_| char::from(b'a' + (next() % 26) as u8))
                    .collect();
                Line::from(s.as_str())
            })
            .collect();
        let round = deserialize_lines(&serialize_lines(&lines));
        assert_eq!(round.len(), lines.len());
        for (a, b) in round.iter().zip(lines.iter()) {
            assert_eq!(a.to_string(), b.to_string());
        }
    }
}

/// `Line::record_is_valid` must agree with `Line::deserialize` on every record,
/// corrupt and truncated ones included. The block decoder debug-asserts the pair
/// on every record it walks; this pins the malformed inputs it never sees.
#[test]
fn record_is_valid_agrees_with_deserialize_on_malformed_records() {
    let mut rle: Rle<CellAttrs> = Rle::new();
    rle.extend_with(CellAttrs::from_raw(1, 2, 0), 4);
    let mut styled = Line::with_hyperlinks(
        "styled line with more than thirty-two content bytes",
        rle,
        vec![HyperlinkSpan::new(
            0,
            4,
            Arc::from("https://example.invalid"),
        )],
    );
    styled.set_wrapped(true);

    let samples = [
        Line::new(),
        Line::from("plain"),
        Line::from("x".repeat(300).as_str()),
        styled,
    ];

    for line in &samples {
        let encoded = line.serialize();
        // Every truncation of a real record, plus the record itself.
        for cut in 0..=encoded.len() {
            let record = &encoded[..cut];
            assert_eq!(
                Line::deserialize(record).is_some(),
                Line::record_is_valid(record),
                "truncated to {cut} of {}",
                encoded.len()
            );
        }
        // Corrupt each header byte (version, flags, and the content length) —
        // the region the predicate reads.
        for idx in 0..encoded.len().min(8) {
            for patch in [0u8, 1, 0x7F, 0xFF] {
                let mut record = encoded.clone();
                record[idx] = patch;
                assert_eq!(
                    Line::deserialize(&record).is_some(),
                    Line::record_is_valid(&record),
                    "byte {idx} patched to {patch:#x}"
                );
            }
        }
    }

    // Arbitrary bytes: a corrupt block hands the decoder records that were
    // never a serialized line at all.
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = move || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (state >> 33) as u32
    };
    for _ in 0..20_000 {
        let len = (next() % 20) as usize;
        // Small byte alphabet so short lengths and zero/one flag bytes (the
        // interesting boundaries) come up often.
        let record: Vec<u8> = (0..len).map(|_| (next() % 5) as u8).collect();
        assert_eq!(
            Line::deserialize(&record).is_some(),
            Line::record_is_valid(&record),
            "record {record:?}"
        );
    }
}

// ── Inline images in history ────────────────────────────────────────────────
//
// These pin the storage contract: `Line` can keep a picture without lying to the
// memory budget. The end-to-end test is aterm-core's
// `tests/image_survives_scrollback.rs`.

/// A `rows`-tall placement whose raster is `payload` bytes.
fn test_image(rows: u16, cols: u16, payload: usize) -> Arc<aterm_types::ImageData> {
    Arc::new(aterm_types::ImageData {
        bytes: vec![0u8; payload],
        format: aterm_types::ImageFormat::Png,
        cols,
        rows,
        z_index: 0,
        band_lift_px: 0,
        cell_offset_px: (0, 0),
        scaling: aterm_types::ImageScaling::Fit,
        source_rect: None,
    })
}

#[test]
fn a_line_reports_the_footprint_tile_each_of_its_columns_paints() {
    let image = test_image(3, 4, 64);
    let mut line = Line::new();
    // Footprint row 1, columns 0..4 of the row, starting at the image's own
    // left edge.
    line.set_images(vec![ImageSpan::new(0, 4, 1, 0, image)]);
    assert!(line.has_images());
    assert_eq!(line.image_count(), 1);
    let span = line.get_image(2).expect("column 2 is covered");
    assert_eq!(span.tile_at(0), Some((1, 0)));
    assert_eq!(span.tile_at(3), Some((1, 3)));
    assert_eq!(span.tile_at(4), None, "end_col is exclusive");
    assert!(line.get_image(4).is_none(), "past the footprint");
}

#[test]
fn a_span_starting_mid_footprint_keeps_its_tile_offsets() {
    // The right half of a footprint row, e.g. after an overwrite ate the left.
    let image = test_image(2, 6, 32);
    let mut line = Line::new();
    line.set_images(vec![ImageSpan::new(10, 13, 0, 3, image)]);
    let span = line.get_image(10).expect("covered");
    assert_eq!(span.tile_at(10), Some((0, 3)));
    assert_eq!(span.tile_at(12), Some((0, 5)));
}

#[test]
fn the_rows_of_one_footprint_together_charge_the_payload_exactly_once() {
    // The accounting the byte budget depends on: a shared payload must not be
    // billed once per covered row (which would report a 1 MiB picture as
    // `rows` MiB and evict history that is not resident), nor billed to nobody.
    const PAYLOAD: usize = 90_000;
    const ROWS: u16 = 9;
    let image = test_image(ROWS, 4, PAYLOAD);
    let bare = Line::new().memory_used();
    let mut total_image_bytes = 0usize;
    for r in 0..ROWS {
        let mut line = Line::new();
        line.set_images(vec![ImageSpan::new(0, 4, r, 0, Arc::clone(&image))]);
        total_image_bytes += line.memory_used() - bare;
    }
    assert!(
        total_image_bytes >= PAYLOAD,
        "the whole footprint under-charges the payload: {total_image_bytes} < {PAYLOAD}"
    );
    // Struct overhead is the only slack: nine spans plus nine boxed SmallVecs,
    // and up to one byte per row lost to the share's integer division.
    let slack = usize::from(ROWS)
        * (std::mem::size_of::<ImageSpan>() + std::mem::size_of::<SmallVec<ImageSpan, 1>>() + 1);
    assert!(
        total_image_bytes <= PAYLOAD + slack,
        "the footprint is billed per row instead of per share: \
         {total_image_bytes} > {PAYLOAD} + {slack}"
    );
}

#[test]
fn serialization_drops_the_image_and_keeps_everything_else() {
    // Pins the scope line: a picture survives in the in-memory hot tier and is
    // dropped when its line is compressed. A block-codec payload table would
    // change this test.
    let rle: Rle<CellAttrs> = Rle::new();
    let mut line = Line::with_hyperlinks("under the image", rle, Vec::new());
    line.set_images(vec![ImageSpan::new(0, 4, 0, 0, test_image(1, 4, 128))]);
    assert!(line.has_images());
    let back = Line::deserialize(&line.serialize()).expect("round trip");
    assert_eq!(
        back.as_str(),
        Some("under the image"),
        "the text still round-trips"
    );
    assert!(
        !back.has_images(),
        "images are deliberately not on the wire — see Line::serialize"
    );
}

/// `clear_hyperlinks` drops the links and nothing else: text, attrs, `WRAPPED`,
/// underline colours and images stay, and the cleared line serializes
/// byte-for-byte like one that never had a link (the wire admits only canonical
/// records).
#[test]
fn clearing_hyperlinks_drops_the_links_and_keeps_everything_else() {
    let red = CellAttrs::new(0x01_FF0000, DEFAULT_BG, 0);
    let styled = || {
        let mut rle: Rle<CellAttrs> = Rle::new();
        for _ in 0..8 {
            rle.push(red);
        }
        rle
    };
    let links = vec![
        HyperlinkSpan::with_id(
            0,
            4,
            Arc::from("https://a.test/long"),
            Some(Arc::from("id-1")),
        ),
        HyperlinkSpan::new(5, 8, Arc::from("https://b.test")),
    ];
    let dress = |line: &mut Line| {
        line.set_wrapped(true);
        line.set_underline_colors(vec![UnderlineColorSpan::new(0, 8, 0x01_10_20_30)]);
        line.set_images(vec![ImageSpan::new(6, 8, 0, 0, test_image(1, 2, 16))]);
    };
    let mut line = Line::with_hyperlinks("abcd efg", styled(), links);
    dress(&mut line);
    let mut never_linked = Line::with_hyperlinks("abcd efg", styled(), Vec::new());
    dress(&mut never_linked);
    assert!(
        line.has_hyperlinks(),
        "PRECONDITION: the line carries links"
    );

    line.clear_hyperlinks();
    assert!(!line.has_hyperlinks());
    assert_eq!(line.hyperlink_count(), 0);
    assert!(
        line.hyperlinks().is_none(),
        "back to None, not an empty vector"
    );
    assert!(line.get_hyperlink(1).is_none());
    assert_eq!(line.as_str(), Some("abcd efg"));
    assert!(line.is_wrapped());
    assert_eq!(line.get_attr(3), red);
    assert_eq!(line.get_attr(7), red);
    assert_eq!(line.get_underline_color(7), Some(0x01_10_20_30));
    assert_eq!(line.image_count(), 1);
    assert!(line.get_image(6).is_some());
    assert_eq!(
        line.serialize(),
        never_linked.serialize(),
        "byte-identical to a line that never had a link"
    );

    // A line with no link is left exactly as it was.
    let before = never_linked.serialize();
    never_linked.clear_hyperlinks();
    assert_eq!(never_linked.serialize(), before);
}

/// The per-line caps the update's history import decodes a `cols`-wide
/// sidecar frame under (`line_caps` in aterm-gui's `handoff_history`): cells,
/// content bytes, and the record cap `16 KiB + cols * 512`.
fn handoff_caps(cols: usize) -> (usize, usize, usize) {
    (cols, cols * 256, 16 * 1024 + cols * 512)
}

fn strict_block(data: &[u8], lines: usize, cols: usize) -> Option<Vec<Line>> {
    let (cells, content, record) = handoff_caps(cols);
    crate::line::line_codec_block::deserialize_lines_strict(data, lines, cells, content, record)
}

fn stripping_block(data: &[u8], lines: usize, cols: usize) -> Option<(Vec<Line>, Vec<usize>)> {
    let (cells, content, record) = handoff_caps(cols);
    crate::line::line_codec_block::deserialize_lines_strict_dropping_over_cap_links(
        data, lines, cells, content, record,
    )
}

/// `links` one-cell OSC 8 links (`L`, then a space) whose URLs are `url_len`
/// bytes, styled red, wrapped, with an underline colour — what a row of
/// long links looks like in a history, and everything a strip must keep.
fn link_dense_row(links: usize, url_len: usize) -> Line {
    let red = CellAttrs::new(0x01_FF0000, DEFAULT_BG, 0);
    let text = "L ".repeat(links);
    let mut rle: Rle<CellAttrs> = Rle::new();
    for _ in text.chars() {
        rle.push(red);
    }
    let spans = (0..links)
        .map(|link| {
            let mut url = format!("https://example.test/{link}/");
            url.push_str(&"u".repeat(url_len.saturating_sub(url.len())));
            let col = u16::try_from(link * 2).expect("a column");
            HyperlinkSpan::with_id(col, col + 1, Arc::from(url.as_str()), Some(Arc::from("i")))
        })
        .collect();
    let mut line = Line::with_hyperlinks(&text, rle, spans);
    line.set_wrapped(true);
    let width = u16::try_from(text.len()).expect("a width");
    line.set_underline_colors(vec![UnderlineColorSpan::new(0, width, 0x01_10_20_30)]);
    line
}

/// A line of `chars` characters, each styled unlike its neighbour: one attrs
/// run per character, so its record is over the handoff's record cap at a
/// narrow width with no link at all.
fn attr_heavy_row(chars: usize) -> Line {
    let a = CellAttrs::new(0x01_FF0000, DEFAULT_BG, 0);
    let b = CellAttrs::new(0x01_00FF00, DEFAULT_BG, 0);
    let mut rle: Rle<CellAttrs> = Rle::new();
    for i in 0..chars {
        rle.push(if i % 2 == 0 { a } else { b });
    }
    Line::with_attrs(&"a".repeat(chars), rle)
}

/// The relaxation only adds leniency: on every block the strict decoder admits,
/// the stripping decoder returns the very same lines and strips none.
#[test]
fn the_stripping_decoder_reads_whatever_the_strict_one_admits_unchanged() {
    let mut short_link = link_dense_row(3, 40);
    short_link.set_wrapped(false);
    let block = serialize_lines(&[
        Line::from("plain"),
        styled_row("styled"),
        short_link,
        link_dense_row(1, 8192),
    ]);
    let strict = strict_block(&block, 4, 80).expect("PRECONDITION: strict admits the block");
    let (lines, stripped) = stripping_block(&block, 4, 80).expect("admitted");
    assert!(stripped.is_empty(), "nothing is stripped: {stripped:?}");
    assert_eq!(serialize_lines(&lines), serialize_lines(&strict));
    assert_eq!(
        serialize_lines(&lines),
        block,
        "and it is the block, byte for byte"
    );
}

/// One row of twelve 8 KiB links is a ~96 KiB record against the 56 KiB cap of
/// an 80-column frame: the strict decoder refuses the block (the negative
/// control); the stripping decoder keeps the line without its links and every
/// other line exactly.
#[test]
fn a_link_dense_line_over_the_cap_keeps_its_text_and_loses_only_its_links() {
    let mut before = link_dense_row(2, 64);
    before.set_wrapped(false);
    let dense = link_dense_row(12, 8192);
    let after = Line::from("after");
    let block = serialize_lines(&[before.clone(), dense.clone(), after.clone()]);
    let (_, _, cap) = handoff_caps(80);
    assert!(
        dense.serialize().len() > cap,
        "PRECONDITION: the dense row's record ({} bytes) is over the {cap}-byte cap",
        dense.serialize().len()
    );
    assert!(
        strict_block(&block, 3, 80).is_none(),
        "NEGATIVE CONTROL: the strict decoder refuses the whole block"
    );

    let (lines, stripped) = stripping_block(&block, 3, 80).expect("the block is kept");
    assert_eq!(stripped, vec![1], "exactly the dense line lost its links");
    assert_eq!(lines.len(), 3);
    assert_eq!(
        lines[0].serialize(),
        before.serialize(),
        "the line before, exactly"
    );
    assert!(lines[0].has_hyperlinks(), "its links are kept");
    assert_eq!(
        lines[2].serialize(),
        after.serialize(),
        "the line after, exactly"
    );
    let mut expected = dense;
    expected.clear_hyperlinks();
    assert!(!lines[1].has_hyperlinks());
    assert_eq!(
        lines[1].as_str(),
        Some("L ".repeat(12).as_str()),
        "the text"
    );
    assert!(lines[1].is_wrapped(), "the wrap flag");
    assert_eq!(lines[1].get_underline_color(3), Some(0x01_10_20_30));
    assert_eq!(
        lines[1].serialize(),
        expected.serialize(),
        "text, attrs, flags and underline colours are exact"
    );
}

/// Everything else the strict decoder refuses is still refused: the relaxation
/// is for a record over the cap because of its links, within the ceiling.
#[test]
fn the_stripping_decoder_refuses_every_record_its_links_do_not_explain() {
    let cols = 8;
    let (_, _, cap) = handoff_caps(cols);
    let ceiling = crate::line::line_codec_block::max_linked_record_bytes(cap, cols);
    let refused = |what: &str, lines: &[Line]| {
        let block = serialize_lines(lines);
        assert!(
            strict_block(&block, lines.len(), cols).is_none(),
            "{what}: strict refuses"
        );
        assert!(
            stripping_block(&block, lines.len(), cols).is_none(),
            "{what}: still refused"
        );
    };

    // Over the cap with no link at all: 2048 characters (the content cap at
    // 8 columns), one attrs run each.
    let heavy = attr_heavy_row(2048);
    assert!(heavy.serialize().len() > cap && heavy.serialize().len() <= ceiling);
    refused("over the cap without a link", &[Line::from("x"), heavy]);

    // Over the cap even with its links dropped.
    let red = CellAttrs::new(0x01_FF0000, DEFAULT_BG, 0);
    let mut rle: Rle<CellAttrs> = Rle::new();
    for i in 0..2048 {
        rle.push(if i % 2 == 0 {
            red
        } else {
            CellAttrs::default()
        });
    }
    let linked_heavy = Line::with_hyperlinks(
        &"a".repeat(2048),
        rle,
        vec![HyperlinkSpan::new(0, 1, Arc::from("https://a.test"))],
    );
    refused("over the cap without its links too", &[linked_heavy]);

    // Past the hard ceiling: eleven 8 KiB links in eight columns is more than
    // one maximal link per column, which no ingested row can hold.
    let past = {
        let mut line = link_dense_row(11, 8192);
        line.set_underline_colors(Vec::new());
        let spans: Vec<HyperlinkSpan> = line
            .hyperlinks()
            .expect("links")
            .iter()
            .map(|span| HyperlinkSpan::new(0, 1, Arc::clone(&span.url)))
            .collect();
        Line::with_hyperlinks("L", Rle::new(), spans)
    };
    assert!(
        past.serialize().len() > ceiling,
        "PRECONDITION: past the ceiling"
    );
    refused("a record past the hard ceiling", &[past]);
    // …while eight of them, one per column, are within it and kept.
    let within = {
        let spans: Vec<HyperlinkSpan> = (0..8u16)
            .map(|col| HyperlinkSpan::new(col, col + 1, Arc::from("u".repeat(8192).as_str())))
            .collect();
        Line::with_hyperlinks("LLLLLLLL", Rle::new(), spans)
    };
    let block = serialize_lines(std::slice::from_ref(&within));
    assert!(within.serialize().len() > cap && within.serialize().len() <= ceiling);
    let (lines, stripped) = stripping_block(&block, 1, cols).expect("within the ceiling");
    assert_eq!((lines[0].as_str(), stripped), (Some("LLLLLLLL"), vec![0]));

    // Over the content cap, links or not.
    let mut long = link_dense_row(3, 8192);
    long.set_underline_colors(Vec::new());
    let text = "a".repeat(3000);
    let long = Line::with_hyperlinks(&text, Rle::new(), long.hyperlinks().unwrap().to_vec());
    refused("content over the content cap", &[long]);

    // Over the cap because of bytes the decode drops, not its links: one short
    // link and 8,000 underline spans at 80 columns. `Line::deserialize` keeps
    // only `MAX_UNDERLINE_SPANS` of them, so judged on the decoded line, dropping
    // the link would fit the cap; the record is not canonical, so it is refused.
    let smuggler = |linked: bool| {
        let spans = if linked {
            vec![HyperlinkSpan::new(0, 1, Arc::from("https://a.test"))]
        } else {
            Vec::new()
        };
        let mut line = Line::with_hyperlinks("L", Rle::new(), spans);
        line.set_underline_colors(vec![UnderlineColorSpan::new(0, 1, 0x01_10_20_30); 8000]);
        line
    };
    let (_, _, cap80) = handoff_caps(80);
    let linked = smuggler(true);
    let record = linked.serialize();
    assert!(
        record.len() > cap80
            && record.len() <= crate::line::line_codec_block::max_linked_record_bytes(cap80, 80),
        "PRECONDITION: over the cap, within the ceiling ({} bytes)",
        record.len()
    );
    let mut decoded = Line::deserialize(&record).expect("PRECONDITION: it decodes");
    assert_eq!(
        decoded.underline_colors().map(<[UnderlineColorSpan]>::len),
        Some(crate::line::line_codec::MAX_UNDERLINE_SPANS),
        "PRECONDITION: the decode truncates the underline spans"
    );
    decoded.clear_hyperlinks();
    assert!(
        decoded.serialize().len() <= cap80,
        "PRECONDITION: judged on the decoded line, dropping the link would fit the cap"
    );
    for (what, line) in [
        ("underline spans the decode drops, with a link", linked),
        ("the same record with no link", smuggler(false)),
    ] {
        let block = serialize_lines(&[Line::from("x"), line]);
        assert!(
            strict_block(&block, 2, 80).is_none(),
            "{what}: strict refuses"
        );
        assert!(
            stripping_block(&block, 2, 80).is_none(),
            "{what}: still refused"
        );
    }

    // A link-dense block with a byte after its last record, or one record
    // short of its count.
    let block = serialize_lines(&[Line::from("x"), link_dense_row(12, 8192)]);
    let mut trailing = block.clone();
    trailing.push(0);
    assert!(
        stripping_block(&trailing, 2, 80).is_none(),
        "a trailing byte"
    );
    let mut short = block;
    short[..4].copy_from_slice(&3u32.to_le_bytes());
    assert!(stripping_block(&short, 3, 80).is_none(), "a missing record");
}

/// A bounded block is wire-identical at its exact budget and refuses smaller
/// budgets, including the styled, linked and underline-colour sections.
#[test]
fn bounded_line_blocks_preserve_wire_bytes_at_their_budget() {
    let mut attrs = Rle::new();
    attrs.push(CellAttrs::new(0x01_FF_00_00, DEFAULT_BG, 0));
    let mut styled = Line::with_hyperlinks(
        "x",
        attrs,
        vec![HyperlinkSpan::with_id(
            0,
            1,
            Arc::from("https://example.test/"),
            Some(Arc::from("id")),
        )],
    );
    styled.set_underline_colors(vec![UnderlineColorSpan::new(0, 1, 0x01_AA_BB_CC)]);
    for lines in [vec![], vec![Line::from("plain")], vec![styled]] {
        let expected = serialize_lines(&lines);
        let bounded = serialize_lines_bounded(&lines, expected.len()).expect("exact budget");
        assert_eq!(bounded, expected);
        assert!(serialize_lines_bounded(&lines, expected.len() - 1).is_none());
        assert!(serialize_lines_bounded(&lines, 0).is_none());
        assert_eq!(deserialize_lines(&bounded).len(), lines.len());
    }
}
