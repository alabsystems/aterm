// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn line_from_str() {
    let line = Line::from("Hello, World!");
    assert_eq!(line.to_string(), "Hello, World!");
    assert_eq!(line.len(), 13);
    assert!(!line.is_empty());
}

#[test]
fn line_empty() {
    let line = Line::new();
    assert!(line.is_empty());
    assert_eq!(line.len(), 0);
}

#[test]
fn line_wrapped_flag() {
    let mut line = Line::from("test");
    assert!(!line.is_wrapped());
    line.set_wrapped(true);
    assert!(line.is_wrapped());
    line.set_wrapped(false);
    assert!(!line.is_wrapped());
}

#[test]
fn line_serialize_roundtrip() {
    let mut line = Line::from("Hello, World!");
    line.set_wrapped(true);

    let serialized = line.serialize();
    let deserialized = Line::deserialize(&serialized).unwrap();

    assert_eq!(deserialized.to_string(), "Hello, World!");
    assert!(deserialized.is_wrapped());
}

#[test]
fn serialize_lines_roundtrip() {
    let lines: Vec<Line> = (0..10).map(|i| Line::from(&*format!("Line {i}"))).collect();

    let serialized = serialize_lines(&lines);
    let deserialized = deserialize_lines(&serialized);

    assert_eq!(deserialized.len(), 10);
    for (i, line) in deserialized.iter().enumerate() {
        assert_eq!(line.to_string(), format!("Line {i}"));
    }
}

/// Regression (round 12): `deserialize_lines` must not retain a Vec whose
/// capacity is driven by an untrusted, inflated `count`. A crafted block can
/// declare a huge line count, contain one valid line, then stop — the old code
/// reserved `count.min((len-4)/5)` slots (~16K+ `Line` structs for an ~82 KiB
/// page) and never shrank, so when the page was cached by DiskColdTier the spare
/// capacity was pinned but uncounted by page_byte_size (len-based), defeating
/// cache_byte_limit. The reservation is now capped and the Vec shrunk to fit.
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

#[test]
fn line_content_inline() {
    let short = LineContent::from_bytes(b"short");
    assert!(matches!(short, LineContent::Inline(_)));
    assert_eq!(short.len(), 5);
}

#[test]
fn line_content_heap() {
    let long_data = vec![b'x'; 200];
    let long = LineContent::from_bytes(&long_data);
    assert!(matches!(long, LineContent::Heap(_)));
    assert_eq!(long.len(), 200);
}

#[test]
fn line_memory_used() {
    let line = Line::from("test");
    assert!(line.memory_used() > 0);
}

// ============================================================================
// RLE Attribute Tests
// ============================================================================

#[test]
fn cell_attrs_default() {
    let attrs = CellAttrs::DEFAULT;
    assert!(attrs.is_default());
    assert_eq!(attrs.fg, DEFAULT_FG);
    assert_eq!(attrs.bg, DEFAULT_BG);
    assert_eq!(attrs.flags, 0);
}

#[test]
fn cell_attrs_serialize_roundtrip() {
    let attrs = CellAttrs::new(0x01_FF0000, 0x01_00FF00, 0x0007);
    let serialized = attrs.serialize();
    let deserialized = CellAttrs::deserialize(&serialized).unwrap();
    assert_eq!(attrs, deserialized);
}

#[test]
fn line_with_attrs() {
    let mut rle: Rle<CellAttrs> = Rle::new();
    // Simulate: 5 chars with red fg, 5 chars with default
    let red_attrs = CellAttrs::new(0x01_FF0000, DEFAULT_BG, 0);
    for _ in 0..5 {
        rle.push(red_attrs);
    }
    for _ in 0..5 {
        rle.push(CellAttrs::DEFAULT);
    }

    let line = Line::with_attrs("HelloWorld", rle);
    assert!(line.has_attrs());
    assert_eq!(line.attr_run_count(), 2);

    // Check attrs at specific positions
    assert_eq!(line.get_attr(0).fg, 0x01_FF0000);
    assert_eq!(line.get_attr(4).fg, 0x01_FF0000);
    assert_eq!(line.get_attr(5).fg, DEFAULT_FG);
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
fn hyperlink_span_contains() {
    let span = HyperlinkSpan::new(5, 15, Arc::from("https://example.com"));
    assert!(!span.contains(4));
    assert!(span.contains(5));
    assert!(span.contains(10));
    assert!(span.contains(14));
    assert!(!span.contains(15));
    assert_eq!(span.width(), 10);
}

#[test]
fn line_with_hyperlinks_basic() {
    let rle: Rle<CellAttrs> = Rle::new();
    let hyperlinks = vec![HyperlinkSpan::new(0, 5, Arc::from("https://example.com"))];
    let line = Line::with_hyperlinks("Click here!", rle, hyperlinks);

    assert!(line.has_hyperlinks());
    assert_eq!(line.hyperlink_count(), 1);
    assert_eq!(
        line.get_hyperlink(0).map(|s| s.as_ref()),
        Some("https://example.com")
    );
    assert_eq!(
        line.get_hyperlink(4).map(|s| s.as_ref()),
        Some("https://example.com")
    );
    assert!(line.get_hyperlink(5).is_none());
    assert!(line.get_hyperlink(10).is_none());
}

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
fn line_no_hyperlinks() {
    let line = Line::from("No links here");
    assert!(!line.has_hyperlinks());
    assert_eq!(line.hyperlink_count(), 0);
    assert!(line.get_hyperlink(0).is_none());
}

#[test]
fn deserialize_caps_oversized_hyperlink_url_and_id() {
    // A crafted checkpoint / .dtrm page can declare a hyperlink span whose
    // url_len/id_len far exceed what OSC 8 ingestion would ever admit
    // (MAX_HYPERLINK_URL_BYTES=8192, id<=256). MAX_LINE_CONTENT_BYTES bounds only
    // the CONTENT section, so without a hyperlink-section cap the deserializer
    // would Arc::from a multi-MiB URL — a stored Line breaching the 2 MiB content
    // invariant. Deserialize must skip an over-cap URL span and drop an over-cap id.
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
    // A crafted Line can declare up to u16::MAX (65535) hyperlink spans; each is
    // ~40 B in memory, so an uncapped rebuild balloons one line's span SmallVec to
    // ~2.6 MiB (the 2 MiB content cap does NOT cover the hyperlink section). A legit
    // row has <= MAX_GRID_COLS coalesced spans, so deserialize caps the reconstructed
    // count at MAX_HYPERLINK_SPANS.
    use crate::line::line_codec::MAX_HYPERLINK_SPANS;

    // Build MANY 1-column spans (well over the cap) at distinct columns.
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
fn line_hyperlink_serialize_roundtrip() {
    let rle: Rle<CellAttrs> = Rle::new();
    let hyperlinks = vec![
        HyperlinkSpan::new(0, 10, Arc::from("https://example.com/path?query=1")),
        HyperlinkSpan::new(15, 25, Arc::from("mailto:test@example.com")),
    ];
    let line = Line::with_hyperlinks("Visit our site or email us!", rle, hyperlinks);

    let serialized = line.serialize();
    let deserialized = Line::deserialize(&serialized).unwrap();

    assert_eq!(deserialized.to_string(), "Visit our site or email us!");
    assert!(deserialized.has_hyperlinks());
    assert_eq!(deserialized.hyperlink_count(), 2);
    assert_eq!(
        deserialized.get_hyperlink(5).map(|s| s.as_ref()),
        Some("https://example.com/path?query=1")
    );
    assert_eq!(
        deserialized.get_hyperlink(20).map(|s| s.as_ref()),
        Some("mailto:test@example.com")
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
    // The common path is byte-identical to before: no SGR 58 → version 3, no
    // trailing section, so a v3 reader is unaffected (backward/forward compat).
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
fn line_underline_colors_and_hyperlinks_roundtrip_together() {
    // Underline colours are appended AFTER hyperlinks; deserialize must locate the
    // section past the hyperlink bytes (offset threading), so both survive.
    let rle: Rle<CellAttrs> = Rle::new();
    let hyperlinks = vec![HyperlinkSpan::with_id(
        0,
        5,
        Arc::from("https://both.test"),
        Some(Arc::from("id-7")),
    )];
    let mut line = Line::with_hyperlinks("BOTHx", rle, hyperlinks);
    line.set_underline_colors(vec![UnderlineColorSpan::new(0, 5, 0x01_10_20_30)]);

    let serialized = line.serialize();
    assert_eq!(serialized[0], 4);
    let d = Line::deserialize(&serialized).unwrap();
    assert_eq!(
        d.get_hyperlink(2).map(|s| s.as_ref()),
        Some("https://both.test")
    );
    assert_eq!(d.get_underline_color(2), Some(0x01_10_20_30));
    assert_eq!(d.get_underline_color(4), Some(0x01_10_20_30));
}

#[test]
fn serialize_lines_underline_colors_block_framing() {
    // Block-level framing regression: a v4 (underline-coloured) line SANDWICHED
    // between other lines. line_size_v1v2 must include the underline section, or
    // `offset` lands mid-section and every following record misframes — the exact
    // corruption the RAM-tier parity tests can't see (warm/cold tier + checkpoint).
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
    // Underline colours with ZERO hyperlinks: the section begins right after the
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

#[test]
fn hyperlink_span_serialized_size() {
    let span = HyperlinkSpan::new(0, 10, Arc::from("https://x.com"));
    // v3: start_col (2) + end_col (2) + url_len (4) + url (13) + id_len (4) = 25
    assert_eq!(span.serialized_size(), 2 + 2 + 4 + 13 + 4);

    // With an explicit ID
    let span_with_id =
        HyperlinkSpan::with_id(0, 10, Arc::from("https://x.com"), Some(Arc::from("link-1")));
    // v3: 2 + 2 + 4 + 13 + 4 + 6 = 31
    assert_eq!(span_with_id.serialized_size(), 2 + 2 + 4 + 13 + 4 + 6);
}

// =============================================================================
// Robustness tests for untrusted input
// =============================================================================

/// Verifies that deserialize_lines clamps with_capacity to data-derived maximum.
///
/// A crafted count=1M with only 4 bytes of actual data cannot contain any lines,
/// so capacity is clamped to 0 (data_remaining / MIN_LINE_SIZE = 0 / 5 = 0).
#[test]
fn deserialize_lines_malicious_count_over_allocates() {
    let count: u32 = 1_000_000;
    let data = count.to_le_bytes();
    let result = deserialize_lines(&data);
    assert!(
        result.is_empty(),
        "huge count with no data should produce empty vec"
    );
    assert_eq!(
        result.capacity(),
        0,
        "capacity clamped to 0 when no data for actual lines"
    );
}

/// deserialize_lines with count=0 and empty data should return empty vec.
#[test]
fn deserialize_lines_zero_count() {
    let data = 0u32.to_le_bytes();
    let result = deserialize_lines(&data);
    assert!(result.is_empty());
}

/// deserialize_lines with truncated data (< 4 bytes) should return empty vec.
#[test]
fn deserialize_lines_truncated_header() {
    let result = deserialize_lines(&[0x01, 0x02]);
    assert!(result.is_empty());
}

/// Fix F: a record declaring a content length past `MAX_LINE_CONTENT_BYTES` is
/// REJECTED, not materialized. A legitimate line (well under the cap) round-trips.
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

/// Fix G: deserialize_lines_capped bounds the RECONSTRUCTED line count so a crafted
/// block of many tiny records cannot amplify into a huge Vec<Line>. The uncapped
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
        "a compact RLE run cannot claim more cells than meta.cols"
    );
}

/// deserialize_lines with count > actual lines should only return what exists.
#[test]
fn deserialize_lines_count_exceeds_actual() {
    // Serialize one real line
    let line = Line::from("test");
    let serialized = serialize_lines(&[line]);
    // Corrupt the count to 100 (actual = 1)
    let mut corrupted = serialized.clone();
    corrupted[0..4].copy_from_slice(&100u32.to_le_bytes());
    let result = deserialize_lines(&corrupted);
    assert_eq!(result.len(), 1, "should only parse the one actual line");
    assert_eq!(result[0].to_string(), "test");
}

/// Fuzz crash artifact: exercises integer overflow in deserialization.
///
/// Crash-24c1eee0 (41 bytes) — should not panic on any code path.
/// Filed as #4950: line_codec integer overflow in Line::deserialize.
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

    // Phase 2: round-trip is vacuously satisfied (empty lines).

    // Phase 3: Line::deserialize on full data — version 0x31 is treated as
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

/// #5860: serialize_lines pre-allocates capacity from content sizes.
///
/// The initial allocation should cover plain-text lines without reallocation.
/// Content-aware estimate: 4 + sum(line.len()) + 9 * line_count.
#[test]
fn serialize_lines_capacity_covers_plain_text() {
    let lines: Vec<Line> = (0..100)
        .map(|i| Line::from(&*format!("Line {:04}: {}", i, "x".repeat(70))))
        .collect();

    let content_bytes: usize = lines.iter().map(|l| l.len()).sum();
    let estimated_cap = 4 + content_bytes + lines.len() * 9;

    let serialized = serialize_lines(&lines);

    // Plain-text lines (no attrs, no hyperlinks) should fit within
    // the content-aware estimate — zero reallocations on the hot path.
    assert!(
        serialized.len() <= estimated_cap,
        "serialized size {} exceeds content-aware estimate {}: \
         each plain-text line should be content + 9 bytes overhead",
        serialized.len(),
        estimated_cap
    );

    // Round-trip correctness preserved.
    let deserialized = deserialize_lines(&serialized);
    assert_eq!(deserialized.len(), 100);
    for (i, line) in deserialized.iter().enumerate() {
        let expected = format!("Line {:04}: {}", i, "x".repeat(70));
        assert_eq!(line.to_string(), expected);
    }
}

/// #5860: serialize_lines handles empty and single-line blocks correctly.
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
fn fuzz_deserialize_lines_never_panics_on_corrupt_disk_data() {
    // Scrollback pages are read back from disk files that may be truncated or
    // bit-flipped. `deserialize_lines` must NEVER panic on arbitrary bytes, and
    // must not over-allocate from an attacker-controlled count header (the
    // capacity is clamped to what the data can hold). This deterministic fuzz
    // sweeps 100k pseudo-random byte sequences — including ones with a huge
    // leading count header — and checks no panic + a bounded result.
    let mut state: u64 = 0x6A09_E667_F3BC_C908;
    let mut next = move || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (state >> 33) as u32
    };
    for _ in 0..100_000 {
        let len = (next() % 256) as usize;
        let mut data: Vec<u8> = (0..len).map(|_| (next() & 0xFF) as u8).collect();
        // Half the time, force a maximal count header to exercise the clamp.
        if len >= 4 && next() & 1 == 0 {
            data[0..4].copy_from_slice(&u32::MAX.to_le_bytes());
        }
        let lines = deserialize_lines(&data);
        // No over-allocation / over-read: the minimum line record is 5 bytes, so
        // a payload of `len` bytes can hold at most `(len-4)/5` lines.
        let max_possible = len.saturating_sub(4) / 5;
        assert!(
            lines.len() <= max_possible,
            "decoded {} lines from {len} bytes (max {max_possible}) — count clamp breached",
            lines.len()
        );
    }
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
