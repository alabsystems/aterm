// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Typed serde: `from_str::<T>` and `to_string` over `#[derive]` models shaped like
//! aterm's.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::fuzz::Lcg;

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

/// Unknown keys are an error where the model asks for it (`deny_unknown_fields` on
/// `Config`), so a typo does not silently do nothing.
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

/// A fixed-length target takes the whole array or refuses it: `[1, 2, 3]` into `[u32; 2]`
/// is an error, not `[1, 2]`. A `Vec` drains every element and is untouched.
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

/// A unit variant spelled as a one-key table carries nothing: `cursor_style = { bar = {} }`
/// is `Bar`, and `cursor_style = { bar = 99 }` is an error, not `Bar` with the `99` dropped.
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

/// `Table: FromStr` and `Value` indexing: the untyped path the art-asset suites read a
/// document through (`doc["layer"][0]["role"]`), with no derived model.
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

/// A schema error in a node with no span of its own (a table made by a dotted key or
/// implied by a deeper header, an array of tables) is located on the key that names it.
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

// ---- round-trip properties --------------------------------------------------

/// Write `value` as TOML and read it back: `Ok` when what comes back equals
/// it, else what went wrong.
fn reads_back<T>(value: &T) -> Result<(), String>
where
    T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let text = aterm_toml::to_string(value).map_err(|e| format!("{value:?} was refused: {e}"))?;
    let back =
        aterm_toml::from_str::<T>(&text).map_err(|e| format!("{text:?} did not read back: {e}"))?;
    if back == *value {
        Ok(())
    } else {
        Err(format!("{text:?} read back as {back:?}, not {value:?}"))
    }
}

/// A key drawn where a spelling could go wrong: the type's extremes, zero,
/// small values of both signs, and wide random ones.
trait DrawKey: Sized {
    fn draw(rng: &mut Lcg) -> Self;
}

macro_rules! draw_integer_key {
    ($($ty:ty),*) => {$(
        impl DrawKey for $ty {
            fn draw(rng: &mut Lcg) -> Self {
                match rng.below(5) {
                    0 => <$ty>::MIN,
                    1 => <$ty>::MAX,
                    2 => 0,
                    3 => (rng.below(20) as $ty).wrapping_sub(10),
                    _ => {
                        let bits = (0..4).fold(0u128, |acc, _| (acc << 32) | u128::from(rng.next()));
                        let wide = bits as $ty;
                        if rng.below(2) == 0 { wide } else { wide.wrapping_neg() }
                    }
                }
            }
        }
    )*};
}

draw_integer_key!(
    u8, i8, u16, i16, u32, i32, u64, i64, u128, i128, usize, isize
);

/// Characters a key spelling can trip on: digits and signs, a dot (a path
/// separator when bare), the escaped ones, and multi-byte ones.
const KEY_CHARS: &[char] = &[
    'a', '7', '0', '-', '+', '.', ' ', '"', '\'', '\\', '\n', '\u{0}', '\u{7f}', 'é', '世', '😀',
];

impl DrawKey for bool {
    fn draw(rng: &mut Lcg) -> Self {
        rng.below(2) == 0
    }
}

impl DrawKey for char {
    fn draw(rng: &mut Lcg) -> Self {
        KEY_CHARS[rng.below(KEY_CHARS.len() as u32) as usize]
    }
}

impl DrawKey for String {
    /// Includes text that LOOKS like another key type's spelling: a string
    /// key stays the string it is.
    fn draw(rng: &mut Lcg) -> Self {
        const LOOKALIKES: &[&str] = &["7", "007", "-0", "+7", "true", "True", "1e3", ""];
        if rng.below(3) == 0 {
            return LOOKALIKES[rng.below(LOOKALIKES.len() as u32) as usize].to_owned();
        }
        (0..rng.below(5)).map(|_| char::draw(rng)).collect()
    }
}

impl<K: DrawKey> DrawKey for Option<K> {
    /// Only `Some`: a `None` key has no spelling and is refused when written.
    fn draw(rng: &mut Lcg) -> Self {
        Some(K::draw(rng))
    }
}

/// Newtype keys, as a model wraps an id, a name, or another newtype.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct KeyId(u32);
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct KeySigned(i64);
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct KeyFlag(bool);
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct KeyName(String);
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct KeyOuter(KeyId);

/// A unit-variant key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
enum KeyLane {
    Fast,
    #[serde(rename = "7")]
    Seven,
}

impl DrawKey for KeyId {
    fn draw(rng: &mut Lcg) -> Self {
        Self(u32::draw(rng))
    }
}
impl DrawKey for KeySigned {
    fn draw(rng: &mut Lcg) -> Self {
        Self(i64::draw(rng))
    }
}
impl DrawKey for KeyFlag {
    fn draw(rng: &mut Lcg) -> Self {
        Self(bool::draw(rng))
    }
}
impl DrawKey for KeyName {
    fn draw(rng: &mut Lcg) -> Self {
        Self(String::draw(rng))
    }
}
impl DrawKey for KeyOuter {
    fn draw(rng: &mut Lcg) -> Self {
        Self(KeyId::draw(rng))
    }
}
impl DrawKey for KeyLane {
    fn draw(rng: &mut Lcg) -> Self {
        if rng.below(2) == 0 {
            Self::Fast
        } else {
            Self::Seven
        }
    }
}

/// A document whose tables are keyed by `K`: one flat, one nested under a key
/// of the same type, and the same map at the document root.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Keyed<K: Ord> {
    flat: BTreeMap<K, i64>,
    nested: BTreeMap<K, BTreeMap<K, i64>>,
}

/// The same table read back through serde's buffer rather than straight from
/// the document: flattened into a struct, as an untagged enum's payload, and
/// inside an internally tagged one. No drawn key spells `name` or `kind`.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct FlatKeyed<K: Ord> {
    name: String,
    #[serde(flatten)]
    rest: BTreeMap<K, i64>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
enum UntaggedKeyed<K: Ord> {
    Map(BTreeMap<K, i64>),
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
enum TaggedKeyed<K: Ord> {
    Rec { map: BTreeMap<K, i64> },
}

/// `rounds` documents keyed by `K`, each written and read back.
fn every_table_reads_back<K>(rng: &mut Lcg, rounds: u32)
where
    K: DrawKey + Ord + Clone + Serialize + serde::de::DeserializeOwned + std::fmt::Debug,
{
    for _ in 0..rounds {
        let flat: BTreeMap<K, i64> = (0..rng.below(6))
            .map(|i| (K::draw(rng), i64::from(i)))
            .collect();
        let nested = (0..rng.below(3))
            .map(|_| {
                let inner = (0..rng.below(3))
                    .map(|i| (K::draw(rng), i64::from(i)))
                    .collect();
                (K::draw(rng), inner)
            })
            .collect();
        let outcomes = [
            reads_back(&flat),
            reads_back(&FlatKeyed {
                name: "a".to_owned(),
                rest: flat.clone(),
            }),
            reads_back(&UntaggedKeyed::Map(flat.clone())),
            reads_back(&TaggedKeyed::Rec { map: flat.clone() }),
            reads_back(&Keyed { flat, nested }),
        ];
        for outcome in outcomes {
            if let Err(why) = outcome {
                panic!("a {} key: {why}", std::any::type_name::<K>());
            }
        }
    }
}

/// A TOML key is a string, so a table keyed by anything else — an integer,
/// a `bool`, a `char`, a unit variant, a newtype or `Some` over one of them —
/// is written with its key spelled as text. Every such table the writer
/// accepts reads back as itself, whether the reader takes it straight from the
/// document or from serde's buffer (a flattened map, an untagged or internally
/// tagged enum). It did not: the reader handed every key to `visit_str`, so
/// `7 = "x"` refused to read back into the `HashMap<u32, _>` that wrote it
/// (`invalid type: string "7", expected u32`), and the buffer did the same
/// after the reader was fixed.
#[test]
fn every_table_the_writer_accepts_reads_back() {
    // The negative control first: a key that does NOT read back as itself is
    // caught.

    /// Written with leading zeros, which no integer key is written with.
    #[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
    struct Padded(u32);
    impl Serialize for Padded {
        fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            s.serialize_str(&format!("{:03}", self.0))
        }
    }
    let refused = reads_back(&BTreeMap::from([(Padded(7), 1i64)])).expect_err("007 is not 7");
    assert!(refused.contains("did not read back"), "{refused}");

    let mut rng = Lcg::new(0x0DDB_A11C_0FFE_E5ED);
    every_table_reads_back::<u8>(&mut rng, 150);
    every_table_reads_back::<i8>(&mut rng, 150);
    every_table_reads_back::<u16>(&mut rng, 150);
    every_table_reads_back::<i16>(&mut rng, 150);
    every_table_reads_back::<u32>(&mut rng, 150);
    every_table_reads_back::<i32>(&mut rng, 150);
    every_table_reads_back::<u64>(&mut rng, 150);
    every_table_reads_back::<i64>(&mut rng, 150);
    every_table_reads_back::<u128>(&mut rng, 150);
    every_table_reads_back::<i128>(&mut rng, 150);
    every_table_reads_back::<usize>(&mut rng, 150);
    every_table_reads_back::<isize>(&mut rng, 150);
    every_table_reads_back::<bool>(&mut rng, 150);
    every_table_reads_back::<char>(&mut rng, 150);
    every_table_reads_back::<String>(&mut rng, 150);
    every_table_reads_back::<Option<u32>>(&mut rng, 150);
    every_table_reads_back::<KeyId>(&mut rng, 150);
    every_table_reads_back::<KeySigned>(&mut rng, 150);
    every_table_reads_back::<KeyFlag>(&mut rng, 150);
    every_table_reads_back::<KeyName>(&mut rng, 150);
    every_table_reads_back::<KeyOuter>(&mut rng, 150);
    every_table_reads_back::<KeyLane>(&mut rng, 150);
}

/// A non-string key reads back from exactly the text the writer spells it
/// with, and from nothing else: a spelling it never writes reaches the key
/// type as the string it is, which refuses it, and the error points at the
/// key. `String` and `Value` keys stay text, however numeric they look.
#[test]
fn a_key_reads_back_only_in_the_spelling_the_writer_uses() {
    let map: HashMap<u32, String> = HashMap::from([(7, "x".to_owned())]);
    let text = aterm_toml::to_string(&map).expect("write");
    assert_eq!(text, "7 = \"x\"\n");
    assert_eq!(
        aterm_toml::from_str::<HashMap<u32, String>>(&text).expect("the map that wrote it"),
        map
    );

    for (input, read) in [
        ("7 = 1\n", true),
        ("\"7\" = 1\n", true),
        ("0 = 1\n", true),
        ("4294967295 = 1\n", true),
        ("007 = 1\n", false),
        ("00 = 1\n", false),
        ("\"+7\" = 1\n", false),
        ("-0 = 1\n", false),
        ("-1 = 1\n", false),
        ("\" 7\" = 1\n", false),
        ("7_0 = 1\n", false),
        ("\"7.0\" = 1\n", false),
        ("1e1 = 1\n", false),
        ("0x7 = 1\n", false),
        ("4294967296 = 1\n", false),
        ("\"\" = 1\n", false),
        ("seven = 1\n", false),
    ] {
        let got = aterm_toml::from_str::<BTreeMap<u32, i64>>(input);
        assert_eq!(got.is_ok(), read, "{input:?} into u32 keys: {got:?}");
    }
    for (input, read) in [
        ("-7 = 1\n", true),
        ("-2147483648 = 1\n", true),
        ("-2147483649 = 1\n", false),
        ("-0 = 1\n", false),
        ("-07 = 1\n", false),
        ("--7 = 1\n", false),
        ("- = 1\n", false),
    ] {
        let got = aterm_toml::from_str::<BTreeMap<i32, i64>>(input);
        assert_eq!(got.is_ok(), read, "{input:?} into i32 keys: {got:?}");
    }
    for (input, read) in [
        ("true = 1\n", true),
        ("false = 1\n", true),
        ("True = 1\n", false),
        ("1 = 1\n", false),
    ] {
        let got = aterm_toml::from_str::<BTreeMap<bool, i64>>(input);
        assert_eq!(got.is_ok(), read, "{input:?} into bool keys: {got:?}");
    }

    let padded = "[ids]\n007 = 1\n";
    let error = aterm_toml::from_str::<BTreeMap<String, BTreeMap<u32, i64>>>(padded)
        .expect_err("a padded key");
    assert!(
        error
            .to_string()
            .contains(r#"invalid type: string "007", expected u32"#),
        "{error}"
    );
    assert_eq!(
        error.span().map(|span| &padded[span]),
        Some("007"),
        "{error}"
    );
    let wide = aterm_toml::from_str::<BTreeMap<u8, i64>>("300 = 1\n").expect_err("past u8");
    assert!(
        wide.to_string()
            .contains("invalid value: integer `300`, expected u8"),
        "{wide}"
    );

    let value: aterm_toml::Value = aterm_toml::from_str("7 = 1\ntrue = 2\n").expect("Value");
    assert_eq!(value["7"].as_integer(), Some(1));
    assert_eq!(value["true"].as_integer(), Some(2));
    let names: BTreeMap<String, i64> = aterm_toml::from_str("007 = 1\n-0 = 2\n").expect("text");
    assert_eq!(names.get("007"), Some(&1));
    assert_eq!(names.get("-0"), Some(&2));
}

/// A `None` the writer puts somewhere a value must be written.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
enum Carried {
    Unit,
    One(Option<i64>),
    Two(i64, Option<i64>),
    Rec { x: Option<i64> },
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Holder {
    x: Option<i64>,
}

/// A newtype over a `None`: nothing reads it back from an absent key.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Wrap(Option<i64>);

/// A struct with a flattened field is written as a map, field by field; its
/// flattened enum's newtype payload is a map VALUE, not a field.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct FlatHolder {
    inner: Option<i64>,
    #[serde(flatten)]
    e: Flattened,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
enum Flattened {
    A(Option<i64>),
    B(i64),
    C { deep: Deep },
}

/// Buffered whole when it is a flattened struct variant's field, so its
/// fields cross serde's buffer before they reach the writer.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Deep {
    x: Option<i64>,
    #[serde(flatten)]
    more: BTreeMap<String, i64>,
}

/// Every place a `None` can sit. Where it must be written: array elements
/// (flat and nested), a tuple field, a fixed array, a variant's payload,
/// `Some(None)`, a newtype over `None` (a field's and inside `Some`), a map
/// value, a flattened newtype variant's payload. Where it is a field a table leaves out: the root's, an
/// array-of-tables row's, a struct variant's, a field of a struct written as a
/// map, and one that crossed serde's buffer.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Carrier {
    keep: i64,
    #[serde(default)]
    list: Vec<Option<i64>>,
    #[serde(default)]
    nested: Vec<Vec<Option<i64>>>,
    pair: Option<(i64, Option<i64>)>,
    fixed: Option<[Option<i64>; 2]>,
    carried: Option<Carried>,
    #[serde(default)]
    rows: Vec<Holder>,
    loose: Option<i64>,
    twice: Option<Option<i64>>,
    wrap: Wrap,
    wrapped: Option<Wrap>,
    #[serde(default)]
    map: BTreeMap<String, Option<i64>>,
    flat: Option<FlatHolder>,
}

/// How many sites [`Carrier::draw`] can put an unwritable `None` at.
const NONE_SITES: u32 = 11;

impl Carrier {
    /// Half the values put a `None` at one site that cannot leave it out,
    /// drawn at random; every value holds `None`s where a table leaves them
    /// out.
    fn draw(rng: &mut Lcg) -> (Self, Option<u32>) {
        fn some(rng: &mut Lcg) -> Option<i64> {
            Some(i64::from(rng.next()) - (1 << 30))
        }
        fn maybe(rng: &mut Lcg) -> Option<i64> {
            if rng.below(3) == 0 { None } else { some(rng) }
        }
        fn list(rng: &mut Lcg) -> Vec<Option<i64>> {
            (0..rng.below(4)).map(|_| some(rng)).collect()
        }
        fn hole(rng: &mut Lcg, mut list: Vec<Option<i64>>) -> Vec<Option<i64>> {
            let at = rng.below(list.len() as u32 + 1) as usize;
            list.insert(at, None);
            list
        }
        let mut carrier = Self {
            keep: i64::from(rng.next()),
            list: list(rng),
            nested: (0..rng.below(3)).map(|_| list(rng)).collect(),
            pair: (rng.below(2) == 0).then(|| (i64::from(rng.next()), some(rng))),
            fixed: (rng.below(2) == 0).then(|| [some(rng), some(rng)]),
            carried: match rng.below(5) {
                0 => None,
                1 => Some(Carried::Unit),
                2 => Some(Carried::One(some(rng))),
                3 => Some(Carried::Two(i64::from(rng.next()), some(rng))),
                _ => Some(Carried::Rec { x: maybe(rng) }),
            },
            rows: (0..rng.below(3))
                .map(|_| Holder { x: maybe(rng) })
                .collect(),
            loose: maybe(rng),
            twice: match rng.below(3) {
                0 => None,
                _ => Some(maybe(rng).or(Some(0))),
            },
            wrap: Wrap(some(rng)),
            wrapped: (rng.below(2) == 0).then(|| Wrap(some(rng))),
            map: (0..rng.below(3))
                .map(|i| (format!("k{i}"), some(rng)))
                .collect(),
            flat: (rng.below(2) == 0).then(|| FlatHolder {
                inner: maybe(rng),
                e: match rng.below(3) {
                    0 => Flattened::A(some(rng)),
                    1 => Flattened::B(i64::from(rng.next())),
                    _ => Flattened::C {
                        deep: Deep {
                            x: maybe(rng),
                            more: (0..rng.below(3))
                                .map(|i| (format!("m{i}"), i64::from(i)))
                                .collect(),
                        },
                    },
                },
            }),
        };
        let site = (rng.below(2) == 0).then(|| rng.below(NONE_SITES));
        match site {
            None => {}
            Some(0) => carrier.list = hole(rng, std::mem::take(&mut carrier.list)),
            Some(1) => {
                let row = list(rng);
                carrier.nested.push(hole(rng, row));
            }
            Some(2) => carrier.pair = Some((i64::from(rng.next()), None)),
            Some(3) => {
                carrier.fixed = Some(if rng.below(2) == 0 {
                    [None, some(rng)]
                } else {
                    [some(rng), None]
                })
            }
            Some(4) => carrier.carried = Some(Carried::One(None)),
            Some(5) => carrier.carried = Some(Carried::Two(i64::from(rng.next()), None)),
            Some(6) => carrier.twice = Some(None),
            Some(7) => carrier.wrap = Wrap(None),
            Some(8) => carrier.wrapped = Some(Wrap(None)),
            Some(9) => {
                carrier.map.insert("hole".to_owned(), None);
            }
            Some(_) => {
                carrier.flat = Some(FlatHolder {
                    inner: maybe(rng),
                    e: Flattened::A(None),
                });
            }
        }
        (carrier, site)
    }

    /// Whether a `None` sits where a value must be written: leaving it out
    /// would shift the elements after it, leave a variant with no payload,
    /// read back as a different value, or not read back at all.
    fn holds_an_unwritable_none(&self) -> bool {
        self.list.contains(&None)
            || self.nested.iter().any(|row| row.contains(&None))
            || matches!(self.pair, Some((_, None)))
            || self.fixed.is_some_and(|fixed| fixed.contains(&None))
            || matches!(
                self.carried,
                Some(Carried::One(None) | Carried::Two(_, None))
            )
            || self.twice == Some(None)
            || self.wrap == Wrap(None)
            || self.wrapped == Some(Wrap(None))
            || self.map.values().any(Option::is_none)
            || matches!(
                self.flat,
                Some(FlatHolder {
                    e: Flattened::A(None),
                    ..
                })
            )
    }
}

/// TOML has no null. A `None` FIELD is left out and reads back as the `None`
/// it was; a `None` that leaving out would lose is REFUSED, with an error: an
/// array cannot skip an element without shifting every later one, a map
/// without its entry is another map, a variant cannot drop its payload,
/// `Some(None)` would read back as `None`, and a newtype over `None` would not
/// read back at all. Before, the array's refusal was the same sentinel a
/// `None` field raises, so the enclosing struct swallowed it and dropped the
/// whole field: `{ keep: 1, list: [Some(1), None] }` wrote `keep = 1` and said
/// nothing. The property: every value either reads back as itself or is
/// refused, never written as something else.
#[test]
fn a_none_is_left_out_of_a_table_and_refused_anywhere_else() {
    #[derive(Debug, Serialize)]
    struct Reported {
        keep: i64,
        list: Vec<Option<i64>>,
    }
    let reported = Reported {
        keep: 1,
        list: vec![Some(1), None],
    };
    let error = aterm_toml::to_string(&reported).expect_err("the KI case is refused");
    assert_eq!(
        error.to_string(),
        "TOML has no null: a `None` field is left out, but a `None` array element, map value, \
         variant payload, `Some` or newtype cannot be written"
    );

    // The negative control: the round-trip check catches a silent loss, here a
    // value written without its low bit.
    #[derive(Debug, PartialEq, Deserialize)]
    struct Even(i64);
    impl Serialize for Even {
        fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            s.serialize_i64(self.0 & !1)
        }
    }
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Lossy {
        e: Even,
    }
    let changed = reads_back(&Lossy { e: Even(7) }).expect_err("7 is not 6");
    assert!(changed.contains("read back as"), "{changed}");
    assert_eq!(reads_back(&Lossy { e: Even(6) }), Ok(()));

    let mut rng = Lcg::new(0x5EED_0FA1_10F1);
    let (mut refused, mut written) = (0u32, 0u32);
    let mut at_site = [0u32; NONE_SITES as usize];
    for _ in 0..3_000 {
        let (carrier, site) = Carrier::draw(&mut rng);
        if carrier.holds_an_unwritable_none() {
            let outcome = aterm_toml::to_string(&carrier);
            assert!(
                outcome
                    .as_ref()
                    .is_err_and(|e| e.to_string().contains("no null")),
                "{carrier:?} must be refused, not written as {outcome:?}"
            );
            refused += 1;
        } else {
            if let Err(why) = reads_back(&carrier) {
                panic!("{why}");
            }
            written += 1;
        }
        if let Some(site) = site {
            at_site[site as usize] += 1;
        }
    }
    // Both halves of the property ran on plenty of cases, and every site held
    // its `None` in many of them.
    assert!(
        refused > 1_000 && written > 1_000,
        "{refused} refused, {written} written"
    );
    assert!(at_site.iter().all(|&n| n > 50), "{at_site:?}");
}

/// A `None` field is left out wherever serde sends it: a struct written as a
/// map (it has a flattened field) and a flattened struct reach the writer
/// through `SerializeMap::serialize_field_entry`, and the field reads back as
/// the `None` it was. A `None` map value and a flattened enum's `None` payload
/// reach it through `serialize_entry`, and are refused: the map without the
/// entry, and the struct without the variant, would read back as something
/// else (`{a: None, b: Some(1)}` as `{b: Some(1)}`) or not at all.
#[test]
fn a_none_field_is_left_out_and_a_none_map_value_refused() {
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Flat {
        name: String,
        note: Option<String>,
        #[serde(flatten)]
        extra: Extra,
        #[serde(flatten)]
        rest: BTreeMap<String, i64>,
    }
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Extra {
        comment: Option<String>,
    }
    let flat = Flat {
        name: "a".to_owned(),
        note: None,
        extra: Extra { comment: None },
        rest: BTreeMap::from([("n".to_owned(), 1)]),
    };
    assert_eq!(
        aterm_toml::to_string(&flat).expect("write"),
        "name = \"a\"\nn = 1\n"
    );
    assert_eq!(reads_back(&flat), Ok(()));

    let map = BTreeMap::from([("a".to_owned(), None), ("b".to_owned(), Some(1i64))]);
    let refused = aterm_toml::to_string(&map).expect_err("a None map value");
    assert!(refused.to_string().contains("no null"), "{refused}");
    let some = BTreeMap::from([("b".to_owned(), Some(1i64))]);
    assert_eq!(reads_back(&some), Ok(()));

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct HoldsFlattened {
        keep: i64,
        #[serde(flatten)]
        e: Flattened,
    }
    let payload = HoldsFlattened {
        keep: 1,
        e: Flattened::A(Some(2)),
    };
    assert_eq!(
        aterm_toml::to_string(&payload).expect("write"),
        "keep = 1\nA = 2\n"
    );
    assert_eq!(reads_back(&payload), Ok(()));
    let vanished = HoldsFlattened {
        keep: 1,
        e: Flattened::A(None),
    };
    let refused = aterm_toml::to_string(&vanished).expect_err("a flattened None payload");
    assert!(refused.to_string().contains("no null"), "{refused}");
}
