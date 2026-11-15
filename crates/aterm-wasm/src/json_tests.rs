// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn every_control_character_keeps_its_json_encoding() {
    let expected = [
        r"\u0000", r"\u0001", r"\u0002", r"\u0003", r"\u0004", r"\u0005", r"\u0006", r"\u0007",
        r"\u0008", r"\t", r"\n", r"\u000b", r"\u000c", r"\r", r"\u000e", r"\u000f", r"\u0010",
        r"\u0011", r"\u0012", r"\u0013", r"\u0014", r"\u0015", r"\u0016", r"\u0017", r"\u0018",
        r"\u0019", r"\u001a", r"\u001b", r"\u001c", r"\u001d", r"\u001e", r"\u001f",
    ];
    let text: String = (0_u8..32).map(char::from).collect();
    assert_eq!(json_string(&text), format!("\"{}\"", expected.join("")));
}

#[test]
fn streaming_encoding_preserves_the_destination_and_utf8() {
    let mut output = String::from("[17,");
    write_json_string(&mut output, "é漢🙂\"\\/\u{7f}\u{2028}");
    output.push(']');
    assert_eq!(output, "[17,\"é漢🙂\\\"\\\\/\u{7f}\u{2028}\"]");
    assert_eq!(json_string(""), "\"\"");
}

#[test]
fn every_non_control_unicode_scalar_is_preserved_between_escapes() {
    let mut text = String::new();
    for scalar in (32..=0x10ffff).filter_map(char::from_u32) {
        if matches!(scalar, '"' | '\\') {
            continue;
        }
        text.push(scalar);
        if text.len() >= 4096 {
            let input = format!("\n{text}\t");
            let mut out = String::from("prefix:");
            write_json_string(&mut out, &input);
            assert_eq!(out, format!("prefix:\"\\n{text}\\t\""));
            text.clear();
        }
    }
    assert_eq!(json_string(&text), format!("\"{text}\""));
}

#[test]
#[ignore = "manual native resource measurement; no timing assertion"]
fn json_string_span_cost() {
    let case = std::env::var("ATERM_JSON_BENCH_CASE").unwrap_or_else(|_| "ascii".into());
    let (chunk, encoded) = match case.as_str() {
        "ascii" => ("plain text 012345", "plain text 012345"),
        "unicode" => ("é漢🙂界", "é漢🙂界"),
        "escaped" => ("\"\\\n\r\t\0", "\\\"\\\\\\n\\r\\t\\u0000"),
        _ => panic!("unknown benchmark case"),
    };
    let text = chunk.repeat(256);
    let expected = format!("\"{}\"", encoded.repeat(256));
    for _ in 0..2000 {
        let output = json_string(std::hint::black_box(&text));
        assert_eq!(output, expected);
        std::hint::black_box(output);
    }
    eprintln!(
        "json span case={case} input_bytes={} output_bytes={} iterations=2000",
        text.len(),
        expected.len()
    );
}
