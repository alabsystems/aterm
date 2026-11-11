// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn reference(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if crate::tty::is_active(c) => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[test]
fn escape_spells_rfc_8259_and_the_active_class() {
    for (raw, json) in [
        ("plain", "plain"),
        ("a\"b\\c", "a\\\"b\\\\c"),
        ("tab\tnl\ncr\r", "tab\\tnl\\ncr\\r"),
        ("\u{8}\u{c}\u{1}\u{1b}[2K", "\\b\\f\\u0001\\u001b[2K"),
        ("café 日本 🙂", "café 日本 🙂"),
        ("\u{7f}", "\\u007f"),
        (
            "CWD\u{9b}31m\u{9d}0;t\u{9c}",
            "CWD\\u009b31m\\u009d0;t\\u009c",
        ),
        ("\u{202e}dcba\u{2066}\u{2069}", "\\u202edcba\\u2066\\u2069"),
        ("\u{61c}\u{200e}\u{200f}", "\\u061c\\u200e\\u200f"),
        ("a\u{2028}b\u{2029}", "a\\u2028b\\u2029"),
        (
            "\u{a0}\u{627}\u{200b}\u{2014}",
            "\u{a0}\u{627}\u{200b}\u{2014}",
        ),
        ("", ""),
    ] {
        assert_eq!(escape(raw), json, "{raw:?}");
    }
}

#[test]
fn the_fast_path_is_the_reference() {
    let narrow = (0u32..0x800).filter_map(char::from_u32);
    let wider = (0x800u32..=0x10_ffff).step_by(7).filter_map(char::from_u32);
    let wide = (0x2000u32..0x2100).filter_map(char::from_u32);
    for c in narrow.chain(wider).chain(wide) {
        let s = format!("a{c}b\"{c}");
        assert_eq!(escape(&s), reference(&s), "{:04x}", c as u32);
    }
    let mut into = String::from("k=");
    escape_into(&mut into, "a\u{9b}");
    assert_eq!(into, "k=a\\u009b");
}

#[test]
fn the_output_holds_no_active_byte_and_decodes_back() {
    let raw = "/tmp/CWD\u{1b}]52;c;Q1dE\u{7}\u{1b}[2K\rFAKE\u{9b}31m\u{202e}dcba\u{2028}\u{7f}é";
    let json = escape(raw);
    assert!(!json.chars().any(crate::tty::is_active), "{json:?}");
    assert_eq!(decode(&json), raw);
}

fn decode(json: &str) -> String {
    let mut out = String::new();
    let mut chars = json.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('b') => out.push('\u{8}'),
            Some('f') => out.push('\u{c}'),
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                let u = u32::from_str_radix(&hex, 16).expect("four hex digits");
                out.push(char::from_u32(u).expect("a BMP scalar"));
            }
            Some(other) => out.push(other),
            None => panic!("a dangling backslash"),
        }
    }
    out
}
