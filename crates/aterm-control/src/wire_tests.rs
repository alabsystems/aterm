// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// `visible_char` spells out `char::is_control` (not a `const fn` on
/// stable Rust): a control character, and only one, shows as a space.
#[test]
fn visible_char_blanks_exactly_the_control_characters() {
    for c in (0..=u32::from(char::MAX)).filter_map(char::from_u32) {
        let shown = if c.is_control() { ' ' } else { c };
        assert_eq!(visible_char(c), shown, "{c:?}");
    }
}

#[test]
fn json_member_keeps_arbitrary_keys_and_values_in_one_member() {
    let key = "cwd\",\"injected\":\"\n\u{202e}";
    let raw = "quoted \"value\"\\\n界";
    let reply = format!("{{{}}}", json_str_field(key, raw));
    let value: aterm_json::Value = aterm_json::from_str(&reply).expect("valid JSON");
    assert_eq!(value.as_object().expect("object").len(), 1);
    assert_eq!(value[key].as_str(), Some(raw));
    assert!(!reply.contains(['\n', '\u{202e}']));
}

#[test]
fn wait_timeouts_default_and_clamp() {
    assert_eq!(clamp_wait_ms(1_000), 1_000);
    assert_eq!(clamp_wait_ms(u64::MAX), WAIT_MAX_MS);
    assert_eq!(resolve_wait_timeout_ms(None), WAIT_DEFAULT_MS);
    assert_eq!(resolve_wait_timeout_ms(Some(900_000)), WAIT_MAX_MS);
    assert_eq!(wait_ms("0"), Some(0));
    assert_eq!(wait_ms("+5"), None);
    assert_eq!(wait_ms(""), None);
}

/// `push_visible` collapses first and trims second, under each rule, and
/// never touches what the buffer held.
#[test]
fn push_visible_collapses_then_trims_each_rule() {
    use VisibleTrim::{None, Spaces, Whitespace};
    let cases: &[(&str, VisibleTrim, &str)] = &[
        ("a\x01b\0c", None, "a b c"),
        ("a\u{9b}b", None, "a b"),
        ("plain  ", None, "plain  "),
        ("x\u{a0}  ", Spaces, "x\u{a0}"),
        ("x\u{3000}\x01", Spaces, "x\u{3000}"),
        ("x\x01", Spaces, "x"),
        ("x\u{a0}", Whitespace, "x"),
        ("x\u{3000}", Whitespace, "x"),
        ("x\u{2028}", Whitespace, "x"),
        ("   \0\t", Whitespace, ""),
        ("x\u{3000}\x01", Whitespace, "x"),
        ("  lead", Whitespace, "  lead"),
    ];
    for &(raw, trim, want) in cases {
        let mut out = String::from("keep  ");
        push_visible(&mut out, raw, trim);
        assert_eq!(out, format!("keep  {want}"), "{raw:?} under {trim:?}");
    }
    let mut out = String::from("keep  ");
    push_visible(&mut out, " \x01 ", Whitespace);
    assert_eq!(
        out, "keep  ",
        "a blank row appends nothing and trims nothing before it"
    );

    let raw = "x\u{3000}\x01";
    let trimmed_first: String = raw.trim_end().chars().map(visible_char).collect();
    assert_eq!(
        trimmed_first, "x\u{3000} ",
        "control: the other order keeps the tail"
    );
}

/// The blank rule counts a raw line and its shown form alike, which is what
/// lets `blocktext` count its tail before it frames a row; interior blank rows
/// stay.
#[test]
fn trimmed_len_is_last_nonblank_plus_one() {
    assert_eq!(trimmed_len(["", "", ""].into_iter()), 0, "all blank");
    assert_eq!(trimmed_len(["   ", " "].into_iter()), 0, "spaces are blank");
    assert_eq!(trimmed_len(std::iter::empty()), 0, "no rows");
    assert_eq!(trimmed_len(["a", "b", "c"].into_iter()), 3, "no blank → n");
    assert_eq!(
        trimmed_len(["a", "", "", "b", "", ""].into_iter()),
        4,
        "interior blanks kept, only the tail dropped"
    );
    assert_eq!(trimmed_len(["", "x"].into_iter()), 2, "leading blank kept");

    let raw = ["out", "\u{a0}", "\x01 \t", "\0"];
    let shown: Vec<String> = raw
        .iter()
        .map(|r| {
            let mut s = String::new();
            push_visible(&mut s, r, VisibleTrim::Whitespace);
            s
        })
        .collect();
    assert_eq!(trimmed_len(raw.into_iter()), 1);
    assert_eq!(trimmed_len(shown.iter().map(String::as_str)), 1);
    let spaces_only = raw
        .iter()
        .rposition(|r| !r.trim_matches(' ').is_empty())
        .map_or(0, |i| i + 1);
    assert_eq!(spaces_only, 4, "control: the old rule keeps every row");
}

/// `visible_row` matches a two-pass reading (the engine's line, collapsed,
/// then trimmed) byte for byte on every row, scrolled back or not, and
/// `visible_row_into` appends exactly it after what the buffer held.
#[test]
fn visible_row_into_appends_the_rows_text_reads() {
    let mut term = aterm_core::terminal::Terminal::new(5, 16);
    for i in 0..12 {
        term.process(format!("line {i}\r\n").as_bytes());
    }
    term.process("e\u{301} 界 nb\u{a0}  \r\nwide 界界".as_bytes());
    term.scroll_display(3);
    for r in 0..7 {
        let line = i32::try_from(r)
            .ok()
            .and_then(|row| term.get_line_text(row, Option::None))
            .unwrap_or_default();
        let two_pass: String = line.chars().map(visible_char).collect();
        let want = two_pass.trim_end();
        assert_eq!(visible_row(&term, r), want, "row {r}");
        let mut out = String::from("pre\u{a0} ");
        visible_row_into(&term, r, &mut out);
        assert_eq!(out, format!("pre\u{a0} {want}"), "row {r}");
    }
    assert_eq!(
        visible_row(&term, 4),
        "wide 界界",
        "precondition: the fixture is on screen"
    );
}
