// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Cell, Decimal, FieldSink, FlagWords, JsonSink, ReadCtx, TextSink};
use std::collections::HashSet;

struct Tee {
    text: TextSink,
    json: JsonSink,
    keys: Vec<String>,
}

impl FieldSink for Tee {
    fn count(&mut self, key: &str, v: u64) {
        self.text.count(key, v);
        self.json.count(key, v);
        self.keys.push(key.to_string());
    }

    fn decimal(&mut self, key: &str, v: Decimal) {
        self.text.decimal(key, v);
        self.json.decimal(key, v);
        self.keys.push(key.to_string());
    }

    fn flag(&mut self, key: &str, v: bool) {
        self.text.flag(key, v);
        self.json.flag(key, v);
        self.keys.push(key.to_string());
    }

    fn label(&mut self, key: &str, v: &str) {
        self.text.label(key, v);
        self.json.label(key, v);
        self.keys.push(key.to_string());
    }

    fn absent(&mut self, key: &str, text: &'static str) {
        self.text.absent(key, text);
        self.json.absent(key, text);
        self.keys.push(key.to_string());
    }

    fn pairs(&mut self, key: &str, pairs: &[(&str, Cell<'_>)], empty_text: &'static str) {
        self.text.pairs(key, pairs, empty_text);
        self.json.pairs(key, pairs, empty_text);
        self.keys.push(key.to_string());
    }
}

pub(crate) struct Body {
    /// `OK key=value ...`.
    text: String,
    /// `{"key":value,...}`.
    json: String,
    /// The keys in emitted order.
    pub(crate) keys: Vec<String>,
}

/// One emit call's text line, JSON object and key log.
pub(crate) fn body(emit: impl FnOnce(&mut dyn FieldSink)) -> Body {
    let mut tee = Tee {
        text: TextSink::new("OK", FlagWords::ZeroOne),
        json: JsonSink::new(),
        keys: Vec::new(),
    };
    emit(&mut tee);
    Body {
        text: tee.text.finish(),
        json: tee.json.finish(),
        keys: tee.keys,
    }
}

fn bodies() -> [Body; 3] {
    let ctx = ReadCtx::now();
    [
        body(|s| crate::metrics::emit_summary(&ctx, Some((24, 80, 7)), s)),
        body(|s| crate::metrics::emit_summary(&ctx, None, s)),
        body(|s| crate::metrics::emit_percentiles(&ctx, s)),
    ]
}

/// `key=value` tokens after the head.
pub(crate) fn tokens(text: &str) -> Vec<(&str, &str)> {
    text.split_whitespace()
        .skip(1)
        .map(|token| token.split_once('=').unwrap_or((token, "")))
        .collect()
}

fn string_end(b: &[u8], start: usize) -> usize {
    let mut i = start + 1;
    while b[i] != b'"' {
        if b[i] == b'\\' {
            i += 1;
        }
        i += 1;
    }
    i + 1
}

fn value_end(b: &[u8], start: usize) -> usize {
    let mut depth = 0usize;
    let mut i = start;
    loop {
        match b[i] {
            b'"' => {
                i = string_end(b, i);
                continue;
            }
            b'{' => depth += 1,
            b',' | b'}' if depth == 0 => return i,
            b'}' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
}

/// The members of one JSON object.
pub(crate) fn members(json: &str) -> Vec<(String, &str)> {
    let b = json.as_bytes();
    assert_eq!(b.first(), Some(&b'{'), "{json}");
    let mut out = Vec::new();
    let mut i = 1;
    while b[i] != b'}' {
        if b[i] == b',' {
            i += 1;
        }
        let key_end = string_end(b, i);
        let key: aterm_json::Value = aterm_json::from_str(&json[i..key_end]).expect("a key");
        i = key_end + 1;
        let end = value_end(b, i);
        out.push((
            key.as_str().expect("a string key").to_string(),
            &json[i..end],
        ));
        i = end;
    }
    out
}

fn flatten(json: &str) -> String {
    if json.starts_with('{') {
        members(json)
            .iter()
            .map(|(_, v)| flatten(v))
            .collect::<Vec<_>>()
            .join("/")
    } else {
        json.to_string()
    }
}

fn agree(text: &str, json: &str) -> bool {
    match json.as_bytes().first() {
        Some(b'n') => json == "null" && ["none", "busy", "-"].contains(&text),
        Some(b't') => json == "true" && text == "1",
        Some(b'f') => json == "false" && text == "0",
        Some(b'"') => aterm_json::from_str::<aterm_json::Value>(json)
            .ok()
            .and_then(|v| v.as_str().map(aterm_codec::pct::encode))
            .is_some_and(|label| label == text),
        Some(b'{') => {
            let pairs = members(json);
            if pairs.is_empty() {
                return text == "none" || text.is_empty();
            }
            let joined = pairs
                .iter()
                .map(|(k, v)| {
                    let k = aterm_codec::pct::encode(k)
                        .replace(',', "%2C")
                        .replace(':', "%3A");
                    format!("{k}:{}", flatten(v))
                })
                .collect::<Vec<_>>()
                .join(",");
            joined == text
        }
        _ => json == text,
    }
}

#[test]
fn text_and_json_agree_key_for_key_and_value_for_value() {
    assert!(!agree("1", "false"));
    assert!(!agree("a:1", "{\"a\":2}"));
    assert!(!agree("x:1/2", "{\"x\":{\"a\":1,\"b\":3}}"));
    assert!(!agree("0.50", "0.5"));
    assert!(!agree("none", "0"));
    assert!(agree(
        "x:1/2,y:3/4",
        "{\"x\":{\"a\":1,\"b\":2},\"y\":{\"a\":3,\"b\":4}}"
    ));
    for body in bodies() {
        let text = tokens(&body.text);
        let json = members(&body.json);
        let text_keys: Vec<&str> = text.iter().map(|(k, _)| *k).collect();
        let json_keys: Vec<&str> = json.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(text_keys, body.keys, "{}", body.text);
        assert_eq!(json_keys, body.keys, "{}", body.json);
        for ((key, t), (_, j)) in text.iter().zip(&json) {
            assert!(agree(t, j), "{key}: text {t:?}, JSON {j}");
        }
    }
}

fn law_violations(body: &Body) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for key in &body.keys {
        if !seen.insert(key) {
            out.push(format!("`{key}` is emitted twice"));
        }
    }
    let tokens: Vec<&str> = body.text.split_whitespace().skip(1).collect();
    if tokens.len() != body.keys.len() {
        out.push(format!(
            "{} keys spell {} text tokens",
            body.keys.len(),
            tokens.len()
        ));
    }
    for (token, key) in tokens.iter().zip(&body.keys) {
        match token
            .strip_prefix(key.as_str())
            .and_then(|rest| rest.strip_prefix('='))
        {
            Some(value) if !value.is_empty() => {}
            _ => out.push(format!("`{token}` is not `{key}=<value>`")),
        }
    }
    out
}

#[test]
fn every_key_is_unique_and_every_text_value_is_one_nonempty_token() {
    let bad = body(|s| {
        s.count("frames", 1);
        s.label("owner", "");
        s.count("frames", 2);
    });
    assert_eq!(law_violations(&bad).len(), 2, "{:?}", law_violations(&bad));
    for body in bodies() {
        assert_eq!(law_violations(&body), Vec::<String>::new(), "{}", body.text);
    }
}

fn startup_contract_violations(json: &str) -> (usize, Vec<String>) {
    let mut checked = 0;
    let mut out = Vec::new();
    for (key, raw) in members(json) {
        if key.ends_with("_schema") {
            checked += 1;
            if raw.parse::<u64>().is_err() || raw == "0" {
                out.push(format!("{key}={raw}"));
            }
        } else if key.ends_with("_valid") || key == "last_present_drop_parked" {
            checked += 1;
            if raw != "true" && raw != "false" {
                out.push(format!("{key}={raw}"));
            }
        }
    }
    (checked, out)
}

#[test]
fn schema_markers_are_integers_and_valid_flags_are_bools() {
    let bad = body(|s| {
        s.decimal("startup_phase_schema", Decimal::new(1.0, 2));
        s.count("startup_phase_valid", 1);
    });
    assert_eq!(startup_contract_violations(&bad.json).1.len(), 2);
    let [summary, ..] = bodies();
    let (checked, violations) = startup_contract_violations(&summary.json);
    assert_eq!(violations, Vec::<String>::new(), "{}", summary.json);
    assert!(
        checked >= 2,
        "no schema or flag was found: {}",
        summary.json
    );
}
