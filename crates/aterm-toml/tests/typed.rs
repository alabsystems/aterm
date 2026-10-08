// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Typed serde: the path 43 call sites in aterm take, and the 161
//! `#[derive(Deserialize)]` models behind them.

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
fn a_realistic_config_deserializes() {
    let ours: Config = aterm_toml::from_str(SAMPLE).expect("our parse");

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
}

/// The `Option` contract: an absent key is `None`, and `None` writes no key at
/// all rather than some spelling of null, which TOML does not have.
#[test]
fn absent_keys_and_none_values() {
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
    assert_eq!(ours, "set = 7\n");
    assert_eq!(aterm_toml::from_str::<S>(&ours).expect("reparse"), value);
}

/// Unknown keys must still be an error where the model asks for it — the
/// `deny_unknown_fields` on `Config` is what stops a typo silently doing
/// nothing.
#[test]
fn unknown_fields_are_refused_where_the_model_asks() {
    let text = "font_px = 1.0\nscrollback_lines = 10\nfont_pxx = 2.0\n";
    assert!(
        aterm_toml::from_str::<Config>(text).is_err(),
        "we accepted an unknown field"
    );
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

/// Every scalar shape a model can ask for, and the numeric coercions (integer
/// into a float field, width-narrowing).
#[test]
fn scalar_coercions() {
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
        Scalars {
            i: -8,
            u: 65535,
            big: i64::MAX,
            f: 1.5,
            f32v: 0.25,
            b: false,
            s: "x".to_string(),
            c: 'q',
        }
    );

    // An integer where a float is expected is accepted; an out-of-range
    // narrowing is refused.
    for (probe, accepted) in [
        ("f = 2\n", true),
        ("u = 70000\n", false),
        ("i = 300\n", false),
        ("big = 9223372036854775808\n", false),
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
        assert_eq!(
            aterm_toml::from_str::<One>(probe).is_ok(),
            accepted,
            "{probe:?}"
        );
    }
}

/// The untyped tree is the other half of the 168 call sites: `from_str::<Value>`
/// and `text.parse::<Value>()`.
#[test]
fn the_untyped_tree() {
    let ours: aterm_toml::Value = SAMPLE.parse().expect("ours");

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

/// A fixed-length target takes the WHOLE array or refuses it.
///
/// serde's array and tuple visitors stop after their N elements and leave it
/// to the deserializer to say whether any were left over — serde's own
/// `SeqDeserializer::end` refuses the extra, and so does this tree's JSON
/// reader. This one did not, so `[1, 2, 3]` read into `[u32; 2]` as `[1, 2]`:
/// the third value gone without a word from a config the editor showed as
/// valid. A `Vec` drains every element and is untouched.
#[test]
fn a_fixed_length_target_refuses_a_longer_array() {
    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    struct Fixed {
        pair: Option<[u32; 2]>,
        tuple: Option<(u8, u8)>,
        rows: Option<(Row,)>,
        list: Option<Vec<u8>>,
    }
    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    struct Row {
        x: u8,
    }
    for (text, accepted) in [
        ("pair = [1, 2]\n", true),
        ("pair = [1, 2, 3]\n", false),
        ("pair = [1]\n", false),
        ("tuple = [1, 2]\n", true),
        ("tuple = [1, 2, 3]\n", false),
        ("[[rows]]\nx = 1\n", true),
        ("[[rows]]\nx = 1\n[[rows]]\nx = 2\n", false),
        ("list = [1, 2, 3]\n", true),
    ] {
        let read = aterm_toml::from_str::<Fixed>(text);
        assert_eq!(read.is_ok(), accepted, "{text:?} read as {read:?}");
    }
    let error = aterm_toml::from_str::<Fixed>("pair = [1, 2, 3]\n").unwrap_err();
    assert_eq!(
        error.span(),
        Some(7..16),
        "the error underlines the array: {error}"
    );
}

/// A unit variant spelled as a one-key table carries NOTHING.
///
/// `cursor_style = { bar = {} }` is `Bar`. `cursor_style = { bar = 99 }` is a
/// value with nowhere to go: an error, not `Bar` with the `99` dropped.
#[test]
fn a_unit_variant_in_table_form_carries_nothing() {
    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    struct Style {
        cursor_style: CursorStyle,
    }
    for (text, accepted) in [
        ("cursor_style = \"bar\"\n", true),
        ("cursor_style = { bar = {} }\n", true),
        ("cursor_style = { bar = [] }\n", true),
        ("[cursor_style.bar]\n", true),
        ("cursor_style = { bar = 99 }\n", false),
        ("cursor_style = { bar = { x = 1 } }\n", false),
        ("cursor_style = { bar = [1] }\n", false),
        ("[cursor_style]\nbar = \"x\"\n", false),
    ] {
        let read = aterm_toml::from_str::<Style>(text);
        assert_eq!(read.is_ok(), accepted, "{text:?} read as {read:?}");
    }
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
/// The art-asset suites read a parsed document with `doc["layer"][0]["role"]`,
/// which goes through no derived model, so it is not covered by the typed
/// cases above.
#[test]
fn the_untyped_table_and_index_surface() {
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
    assert_eq!(ours.len(), 4);

    let ours: aterm_toml::Value = aterm_toml::from_str(SOURCE).expect("parse");

    assert_eq!(ours["id"].as_str(), Some("hero"));
    assert_eq!(ours["viewbox"][1].as_integer(), Some(24));
    assert_eq!(ours["anchor"]["eye_y"].as_float(), Some(7.5));
    assert_eq!(ours["layer"][0]["role"].as_str(), Some("body"));
    assert_eq!(ours["layer"][0]["paths"][0].as_str(), Some("M0 0"));

    // A malformed document is refused, with a span.
    let bad = "a = ";
    assert!(bad.parse::<aterm_toml::Table>().is_err());
    let err = bad.parse::<aterm_toml::Table>().unwrap_err();
    assert!(err.span().is_some(), "a refusal carries a span: {err}");
}

/// Indexing a miss panics rather than yielding a default — the reason
/// `Value::get` exists beside it.
#[test]
#[should_panic(expected = "index not found")]
fn indexing_a_missing_key_panics() {
    let value: aterm_toml::Value = aterm_toml::from_str("a = 1").expect("parse");
    let _ = &value["nope"];
}

/// A schema error in a node with no span of its own — a table made by a dotted
/// key or implied by a deeper header, an array of tables — is located on the
/// KEY that names it. Unlocated, the config editor put the diagnostic on line
/// 1 with no key named.
#[test]
fn a_schema_error_in_a_spanless_node_is_located_on_its_key() {
    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    struct Doc {
        font: Option<Font>,
        net: Option<StrictNet>,
        background_material: Option<Material>,
        rows: Option<(Row,)>,
        a: Option<HashMap<String, String>>,
    }
    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    struct Font {
        family: String,
    }
    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    struct StrictNet {
        listen: String,
        timeout_ms: u32,
    }
    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "kebab-case")]
    #[allow(dead_code)]
    enum Material {
        Solid,
        Blur { radius: f32 },
    }
    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    struct Row {
        x: u8,
    }
    // (source, the token the error must underline, and the byte it starts at
    // where that token appears more than once)
    for (text, token, at) in [
        // A dotted table where a string belongs.
        ("font.family.name = \"x\"\n", "family", None),
        // A missing field in a dotted table, and in an implied one.
        ("net.listen = \"x\"\n", "net", None),
        ("[a]\n[net.x]\n", "net", None),
        // An unknown variant names the variant key, in all three spellings.
        ("background_material.glass = {}\n", "glass", None),
        ("[background_material.glass]\n", "glass", None),
        ("background_material = { glass = {} }\n", "glass", None),
        // A missing field in a dotted variant body.
        ("background_material.blur.x = 1\n", "blur", None),
        // An array of tables longer than its target.
        ("[[rows]]\nx = 1\n[[rows]]\nx = 2\n", "rows", Some(2)),
    ] {
        let error = aterm_toml::from_str::<Doc>(text).expect_err(text);
        let span = error
            .span()
            .unwrap_or_else(|| panic!("{text:?}: `{error}` is not located"));
        assert_eq!(
            &text[span.clone()],
            token,
            "{text:?}: `{error}` underlines {span:?}"
        );
        if let Some(at) = at {
            assert_eq!(span.start, at, "{text:?}: `{error}`");
        }
    }
}
