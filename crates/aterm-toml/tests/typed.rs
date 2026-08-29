// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Typed serde: the path 43 call sites in aterm take, and the 161
//! `#[derive(Deserialize)]` models behind them.
//!
//! Every case here is checked against the `toml` oracle with the SAME derived
//! impls, so a disagreement is a disagreement about the deserializer, not about
//! how the test spells the model.

mod common;

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Shaped like the real consumers: a scalar-heavy root, nested tables reached
/// by dotted keys, list-valued keys, optional keys that are absent from most
/// files, an enum spelled as a bare string, and a repeated `[[section]]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    font_px: f64,
    scrollback_lines: u64,
    #[serde(default)]
    blink: bool,
    #[serde(default)]
    palette: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(default)]
    cursor_style: CursorStyle,
    #[serde(default)]
    net: Net,
    #[serde(default)]
    keybind: Vec<Keybind>,
    #[serde(default)]
    env: HashMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum CursorStyle {
    #[default]
    Block,
    Bar,
    Underline,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Net {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    listen: Option<String>,
    #[serde(default)]
    timeout_ms: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Keybind {
    keys: String,
    action: String,
    #[serde(default)]
    args: Vec<i64>,
}

const SAMPLE: &str = r##"
# aterm configuration
font_px = 13.5          # cozy
scrollback_lines = 100_000
blink = true
palette = ["#101010", "#ff5555", '#50fa7b']
cursor_style = "bar"

net.listen = "127.0.0.1:7777"
net.timeout_ms = 2_500

[env]
TERM = "aterm"
COLORTERM = "truecolor"

[[keybind]]
keys = "ctrl+shift+c"
action = "copy"

[[keybind]]
keys = "ctrl+shift+v"
action = "paste"
args = [1, 2, 3]
"##;

#[test]
fn a_realistic_config_deserializes_exactly_as_the_oracle_does() {
    let ours: Config = aterm_toml::from_str(SAMPLE).expect("our parse");
    let theirs: Config = toml::from_str(SAMPLE).expect("oracle parse");
    assert_eq!(ours, theirs);

    assert_eq!(ours.font_px, 13.5);
    assert_eq!(ours.scrollback_lines, 100_000);
    assert_eq!(ours.cursor_style, CursorStyle::Bar);
    assert_eq!(ours.net.listen.as_deref(), Some("127.0.0.1:7777"));
    assert_eq!(ours.keybind.len(), 2);
    assert_eq!(ours.keybind[1].args, vec![1, 2, 3]);
    assert_eq!(ours.env.get("TERM").map(String::as_str), Some("aterm"));
    assert_eq!(ours.title, None);
}

#[test]
fn a_typed_value_survives_serialize_then_deserialize() {
    let original: Config = aterm_toml::from_str(SAMPLE).expect("parse");
    let text = aterm_toml::to_string(&original).expect("serialize");
    let back: Config = aterm_toml::from_str(&text).expect("reparse our own output");
    assert_eq!(
        original, back,
        "our serializer lost or changed something:\n{text}"
    );

    // And the oracle reads our output the same way.
    let theirs: Config = toml::from_str(&text).expect("the oracle reads our output");
    assert_eq!(
        original, theirs,
        "the oracle disagrees about our output:\n{text}"
    );
}

#[test]
fn our_output_and_the_oracles_output_are_the_same_text() {
    let original: Config = aterm_toml::from_str(SAMPLE).expect("parse");
    assert_eq!(
        aterm_toml::to_string(&original).expect("ours"),
        toml::to_string(&original).expect("theirs"),
    );
}

/// The `Option` contract: an absent key is `None`, and `None` writes no key at
/// all rather than some spelling of null, which TOML does not have.
#[test]
fn absent_keys_and_none_values_agree_with_the_oracle() {
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct S {
        set: Option<i64>,
        unset: Option<i64>,
    }
    let value = S {
        set: Some(7),
        unset: None,
    };
    let ours = aterm_toml::to_string(&value).expect("ours");
    assert_eq!(ours, toml::to_string(&value).expect("theirs"));
    assert_eq!(ours, "set = 7\n");
    assert_eq!(aterm_toml::from_str::<S>(&ours).expect("reparse"), value);
}

/// Unknown keys must still be an error where the model asks for it — the
/// `deny_unknown_fields` on `Config` is what stops a typo silently doing
/// nothing.
#[test]
fn unknown_fields_are_refused_where_the_model_asks() {
    let text = "font_px = 1.0\nscrollback_lines = 10\nfont_pxx = 2.0\n";
    let ours = aterm_toml::from_str::<Config>(text);
    let theirs = toml::from_str::<Config>(text);
    assert!(ours.is_err(), "we accepted an unknown field");
    assert!(theirs.is_err(), "the oracle accepted an unknown field");
}

/// Schema errors carry a span, because the config editor underlines them.
#[test]
fn schema_errors_carry_a_source_span() {
    let text = "font_px = 13.5\nscrollback_lines = \"not a number\"\n";
    let error = aterm_toml::from_str::<Config>(text).expect_err("a string is not a u64");
    let span = error.span().expect("a schema error must locate itself");
    assert_eq!(
        &text[span.clone()],
        "\"not a number\"",
        "span {span:?} points at the wrong token"
    );

    let syntax = aterm_toml::from_str::<Config>("font_px = ?\n").expect_err("`?` is not a value");
    assert!(
        syntax.span().is_some(),
        "a syntax error must locate itself too"
    );
}

/// Every scalar shape a model can ask for, cross-checked against the oracle so
/// the numeric coercions (integer into a float field, width-narrowing) match
/// exactly.
#[test]
fn scalar_coercions_match_the_oracle() {
    #[derive(Debug, PartialEq, Deserialize)]
    struct Scalars {
        i: i8,
        u: u16,
        big: i64,
        f: f64,
        f32v: f32,
        b: bool,
        s: String,
        c: char,
    }
    let text = "i = -8\nu = 65535\nbig = 9223372036854775807\nf = 1.5\nf32v = 0.25\nb = false\ns = \"x\"\nc = \"q\"\n";
    assert_eq!(
        aterm_toml::from_str::<Scalars>(text).expect("ours"),
        toml::from_str::<Scalars>(text).expect("theirs"),
    );

    // An integer where a float is expected, and an out-of-range narrowing:
    // whatever the oracle does, we do.
    for probe in [
        "f = 2\n",
        "u = 70000\n",
        "i = 300\n",
        "big = 9223372036854775808\n",
    ] {
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct One {
            #[serde(default)]
            f: f64,
            #[serde(default)]
            u: u16,
            #[serde(default)]
            i: i8,
            #[serde(default)]
            big: i64,
        }
        let ours = aterm_toml::from_str::<One>(probe);
        let theirs = toml::from_str::<One>(probe);
        assert_eq!(
            ours.is_ok(),
            theirs.is_ok(),
            "{probe:?}: we {} but the oracle {}",
            if ours.is_ok() { "accept" } else { "reject" },
            if theirs.is_ok() { "accepts" } else { "rejects" },
        );
    }
}

/// The untyped tree is the other half of the 168 call sites: `from_str::<Value>`
/// and `text.parse::<Value>()`.
#[test]
fn the_untyped_tree_matches_the_oracle() {
    let ours: aterm_toml::Value = SAMPLE.parse().expect("ours");
    let theirs: toml::Value = SAMPLE.parse().expect("theirs");
    common::values_agree(&ours, &theirs).expect("value trees agree");

    assert_eq!(
        ours.get("font_px").and_then(aterm_toml::Value::as_float),
        Some(13.5)
    );
    assert_eq!(
        ours.get("net")
            .and_then(|n| n.get("listen"))
            .and_then(aterm_toml::Value::as_str),
        Some("127.0.0.1:7777")
    );
    assert_eq!(
        ours.get("palette")
            .and_then(aterm_toml::Value::as_array)
            .map(Vec::len),
        Some(3)
    );
}

/// Date-times cross serde through a reserved struct name; that they survive a
/// typed round-trip is the check that the protocol is wired on both sides.
#[test]
fn datetimes_round_trip_through_serde() {
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Stamps {
        offset: aterm_toml::Datetime,
        local: aterm_toml::Datetime,
        date: aterm_toml::Datetime,
        time: aterm_toml::Datetime,
    }
    let text = "offset = 1979-05-27T07:32:00Z\nlocal = 1979-05-27T07:32:00.999\ndate = 1979-05-27\ntime = 07:32:00\n";
    let parsed: Stamps = aterm_toml::from_str(text).expect("parse");
    assert_eq!(parsed.offset.offset, Some(aterm_toml::Offset::Z));
    assert_eq!(parsed.local.time.expect("a time").nanosecond, 999_000_000);
    assert_eq!(parsed.date.time, None);
    assert_eq!(parsed.time.date, None);
    let printed = aterm_toml::to_string(&parsed).expect("serialize");
    assert_eq!(printed, text);
    assert_eq!(
        aterm_toml::from_str::<Stamps>(&printed).expect("reparse"),
        parsed
    );
}

/// `Table: FromStr` and `Value` indexing — the untyped path.
///
/// `aterm-containment` parses its allowlist straight into a `Table`, and the
/// art-asset suites read a parsed document with `doc["layer"][0]["role"]`.
/// Neither goes through a derived model, so neither is covered by the typed
/// cases above; both are checked against the oracle here.
#[test]
fn the_untyped_table_and_index_surface_matches_toml() {
    const SOURCE: &str = "\
id = \"hero\"
viewbox = [0, 24]

[anchor]
eye_y = 7.5

[[layer]]
role = \"body\"
paths = [\"M0 0\"]
";
    let ours: aterm_toml::Table = SOURCE.parse().expect("a document is a table");
    let theirs: toml::Table = SOURCE.parse().expect("the oracle agrees");
    assert_eq!(ours.len(), theirs.len());

    let ours: aterm_toml::Value = aterm_toml::from_str(SOURCE).expect("parse");
    let theirs: toml::Value = toml::from_str(SOURCE).expect("oracle parses");

    assert_eq!(ours["id"].as_str(), theirs["id"].as_str());
    assert_eq!(
        ours["viewbox"][1].as_integer(),
        theirs["viewbox"][1].as_integer()
    );
    assert_eq!(
        ours["anchor"]["eye_y"].as_float(),
        theirs["anchor"]["eye_y"].as_float()
    );
    assert_eq!(
        ours["layer"][0]["role"].as_str(),
        theirs["layer"][0]["role"].as_str()
    );
    assert_eq!(
        ours["layer"][0]["paths"][0].as_str(),
        theirs["layer"][0]["paths"][0].as_str()
    );

    // A malformed document is refused by both, and ours carries a span.
    let bad = "a = ";
    assert!(bad.parse::<aterm_toml::Table>().is_err());
    assert!(bad.parse::<toml::Table>().is_err());
    let err = bad.parse::<aterm_toml::Table>().unwrap_err();
    assert!(err.span().is_some(), "a refusal carries a span: {err}");
}

/// Indexing a miss panics rather than yielding a default — the oracle's
/// contract, and the reason `Value::get` exists beside it.
#[test]
#[should_panic(expected = "index not found")]
fn indexing_a_missing_key_panics() {
    let value: aterm_toml::Value = aterm_toml::from_str("a = 1").expect("parse");
    let _ = &value["nope"];
}
