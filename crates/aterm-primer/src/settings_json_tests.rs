// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_settings_document_round_trips_in_order() {
    let text = r#"{
  "permissions": {
    "allow": ["Bash(git status)", "Read"],
    "deny": []
  },
  "env": {"FOO": "bar"},
  "hooks": {
    "Stop": [{"hooks": [{"type": "command", "command": "/x hook run stop", "timeout": 600}]}]
  },
  "ratio": 1.50,
  "big": 1e3,
  "neg": -0,
  "flag": true,
  "nothing": null
}"#;
    let doc = Json::parse(text).expect("parses");
    let keys: Vec<&str> = doc
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, _)| k.as_str())
        .collect();
    assert_eq!(
        keys,
        [
            "permissions",
            "env",
            "hooks",
            "ratio",
            "big",
            "neg",
            "flag",
            "nothing"
        ]
    );
    assert_eq!(doc.get("ratio"), Some(&Json::Number("1.50".into())));
    assert_eq!(doc.get("big"), Some(&Json::Number("1e3".into())));
    assert_eq!(doc.get("neg"), Some(&Json::Number("-0".into())));
    let again = Json::parse(&doc.render()).expect("the rendering parses");
    assert_eq!(again, doc);
    assert!(doc.render().contains("\"ratio\": 1.50"));
}

#[test]
fn strings_escape_and_unescape() {
    let doc = Json::parse(r#""a\"b\\c\/d\b\f\n\r\t\u00e9\ud83d\ude00 héllo""#).unwrap();
    assert_eq!(
        doc,
        Json::Str("a\"b\\c/d\u{8}\u{c}\n\r\t\u{e9}\u{1F600} héllo".into())
    );
    assert_eq!(string("a\"b\\c\n\u{1b}"), "\"a\\\"b\\\\c\\n\\u001b\"");
    assert_eq!(string("héllo"), "\"héllo\"");
    let round = Json::parse(&Json::Str("tab\there \u{1F600}".into()).render()).unwrap();
    assert_eq!(round, Json::Str("tab\there \u{1F600}".into()));
}

#[test]
fn malformed_documents_are_refused() {
    for bad in [
        "",
        "{",
        "[1,]",
        "{\"a\":}",
        "{\"a\" 1}",
        "01",
        "1.",
        "-",
        "1e",
        "\"unterminated",
        "\"bad\\escape\"",
        "\"ctrl\u{1}\"",
        "\"\\ud83d\"",
        "tru",
        "{} trailing",
        "\"\\u12\"",
        "\"\\u+041\"",
        "\"\\u+0041\"",
    ] {
        assert!(Json::parse(bad).is_err(), "{bad:?} must be refused");
    }
    let deep = format!("{}1{}", "[".repeat(100), "]".repeat(100));
    assert!(Json::parse(&deep).is_err(), "nesting is bounded");
    let fine = format!("{}1{}", "[".repeat(20), "]".repeat(20));
    assert!(Json::parse(&fine).is_ok());
}

#[test]
fn a_key_written_twice_reads_as_json_parse_reads_it() {
    let doc = Json::parse(r#"{"a": 1, "b": {"c": 1, "c": 2}, "a": 3}"#).unwrap();
    assert_eq!(doc.get("a"), Some(&Json::Number("3".into())));
    assert_eq!(
        doc.render(),
        "{\n  \"a\": 3,\n  \"b\": {\n    \"c\": 2\n  }\n}"
    );
    assert_eq!(Json::Object(vec![]).render(), "{}");
}
