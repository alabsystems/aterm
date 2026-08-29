// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Differential oracle: `aterm_json` against the `serde_json` it replaces.
//!
//! Two properties, asserted separately because they fail separately:
//!
//! * **the same verdict.** For any input, both accept or both refuse. A parser
//!   that is merely more permissive is a parser that hands the program a
//!   document the reference would have rejected.
//! * **the same bytes.** Serialization output is compared byte for byte, not
//!   "semantically equal", because some of this JSON is hashed into a
//!   checkpoint fingerprint (`aterm-gui`'s seamless handoff) and some of it is
//!   a request body signed or replayed elsewhere.
//!
//! `serde_json` is a `[dev-dependencies]` entry only: it reaches no shipped
//! binary, and it is here to be disagreed with.

use serde::{Deserialize, Serialize};

// ── helpers ────────────────────────────────────────────────────────────────

/// Both parse to their own `Value`, and both must agree on the verdict AND the
/// tree — walked node by node, since the two `Value` types are distinct.
fn agree_parse(input: &str) {
    let ours = aterm_json::from_str::<aterm_json::Value>(input);
    let theirs = serde_json::from_str::<serde_json::Value>(input);
    match (&ours, &theirs) {
        (Ok(a), Ok(b)) => assert!(
            values_agree(a, b),
            "parsed trees differ for {:?}:\n  ours   {}\n  oracle {}",
            truncate(input),
            aterm_json::to_string(a).unwrap_or_default(),
            serde_json::to_string(b).unwrap_or_default(),
        ),
        (Err(_), Err(_)) => {}
        _ => panic!(
            "verdicts differ for {:?}: ours={:?} oracle={:?}",
            truncate(input),
            ours.as_ref().err().map(ToString::to_string),
            theirs.as_ref().err().map(ToString::to_string),
        ),
    }
}

/// Structural equality between the two `Value` types.
///
/// Exact everywhere EXCEPT the magnitude of a parsed float, where a relative
/// slack is allowed — see [`our_float_parsing_is_correctly_rounded`] for why
/// the slack is on the ORACLE's side and not ours, and for the assertion that
/// pins OUR side to the exact value with no slack at all. The number's KIND
/// (unsigned / signed / float) still has to match exactly, so an integer that
/// one side widened to a double is a failure, not a rounding difference, and
/// the tolerance is tight enough that a wrong digit or a wrong exponent is
/// still a failure.
fn values_agree(ours: &aterm_json::Value, theirs: &serde_json::Value) -> bool {
    use aterm_json::Value as A;
    use serde_json::Value as B;
    match (ours, theirs) {
        (A::Null, B::Null) => true,
        (A::Bool(a), B::Bool(b)) => a == b,
        (A::String(a), B::String(b)) => a == b,
        (A::Number(a), B::Number(b)) => {
            if a.is_u64() != b.is_u64() || a.is_i64() != b.is_i64() || a.is_f64() != b.is_f64() {
                return false;
            }
            match (a.as_u64(), b.as_u64()) {
                (Some(x), Some(y)) => return x == y,
                (None, None) => {}
                _ => return false,
            }
            match (a.as_i64(), b.as_i64()) {
                (Some(x), Some(y)) => return x == y,
                (None, None) => {}
                _ => return false,
            }
            match (a.as_f64(), b.as_f64()) {
                (Some(x), Some(y)) => floats_agree(x, y),
                _ => false,
            }
        }
        (A::Array(a), B::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| values_agree(x, y))
        }
        // Both maps are sorted by key (`BTreeMap`, and `serde_json` without
        // `preserve_order`), so a positional walk compares like for like.
        (A::Object(a), B::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .zip(b.iter())
                    .all(|((ka, va), (kb, vb))| ka == kb && values_agree(va, vb))
        }
        _ => false,
    }
}

/// Two doubles that agree to within the oracle's own float-parsing slack.
///
/// `1e-13` relative is roughly 450 ULPs: loose enough to absorb
/// `serde_json`'s fast-path rounding, tight enough that a wrong digit, a wrong
/// exponent or a dropped sign is still a failure.
fn floats_agree(a: f64, b: f64) -> bool {
    if a == b {
        return true;
    }
    if !a.is_finite() || !b.is_finite() {
        return false;
    }
    if a.is_sign_negative() != b.is_sign_negative() {
        return false;
    }
    (a - b).abs() <= 1e-13 * a.abs().max(b.abs())
}

fn truncate(input: &str) -> String {
    if input.len() <= 200 {
        input.to_string()
    } else {
        format!(
            "{}…",
            &input[..input.char_indices().nth(200).map_or(0, |(i, _)| i)]
        )
    }
}

/// A deterministic pseudo-random source.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u32
    }
    fn below(&mut self, n: u32) -> u32 {
        if n == 0 { 0 } else { self.next() % n }
    }
}

/// Generate a random VALID JSON document, bounded in depth and size.
fn random_json(rng: &mut Rng, depth: u32, out: &mut String) {
    let choice = if depth >= 4 {
        rng.below(5)
    } else {
        rng.below(7)
    };
    match choice {
        0 => out.push_str("null"),
        1 => out.push_str(if rng.next().is_multiple_of(2) {
            "true"
        } else {
            "false"
        }),
        2 => {
            // Numbers across every shape the parser classifies differently.
            match rng.below(7) {
                0 => out.push_str(&rng.next().to_string()),
                1 => out.push_str(&format!("-{}", rng.next())),
                2 => out.push_str(&format!("{}.{}", rng.next(), rng.next())),
                3 => out.push_str(&format!("{}e{}", rng.below(1000), rng.below(60))),
                4 => out.push_str(&format!("{}e-{}", rng.below(1000), rng.below(60))),
                5 => out.push_str(&u64::MAX.to_string()),
                _ => out.push_str(&i64::MIN.to_string()),
            }
        }
        3 => {
            out.push('"');
            for _ in 0..rng.below(12) {
                match rng.below(8) {
                    0 => out.push_str("\\n"),
                    1 => out.push_str("\\\""),
                    2 => out.push_str("\\\\"),
                    3 => out.push_str("\\u00e9"),
                    4 => out.push_str("\\ud83d\\ude00"),
                    5 => out.push('é'),
                    6 => out.push('世'),
                    _ => out.push((b'a' + (rng.below(26) as u8)) as char),
                }
            }
            out.push('"');
        }
        4 => out.push_str("\"\""),
        5 => {
            out.push('[');
            let n = rng.below(5);
            for i in 0..n {
                if i > 0 {
                    out.push(',');
                }
                random_json(rng, depth + 1, out);
            }
            out.push(']');
        }
        _ => {
            out.push('{');
            let n = rng.below(5);
            for i in 0..n {
                if i > 0 {
                    out.push(',');
                }
                // Deliberately a small key space, so duplicate keys happen.
                out.push_str(&format!("\"k{}\":", rng.below(4)));
                random_json(rng, depth + 1, out);
            }
            out.push('}');
        }
    }
}

// ── the properties ─────────────────────────────────────────────────────────

#[test]
fn generated_documents_parse_and_reserialize_identically() {
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    let mut total = 0usize;
    for _ in 0..20_000 {
        let mut doc = String::new();
        random_json(&mut rng, 0, &mut doc);
        total += doc.len();
        agree_parse(&doc);
    }
    eprintln!("generated corpus: 20000 documents, {total} bytes");
}

/// Mutated valid JSON: the verdicts must still line up, which is where a
/// too-permissive parser shows itself.
#[test]
fn mutated_documents_get_the_same_verdict() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    const NOISE: &[u8] = b"{}[],:\"\\ \n\t0129e+-.abcdefnulltruefalse\x01\x7f";
    for _ in 0..40_000 {
        let mut doc = String::new();
        random_json(&mut rng, 0, &mut doc);
        let mut bytes = doc.into_bytes();
        if bytes.is_empty() {
            continue;
        }
        for _ in 0..=rng.below(3) {
            let at = (rng.next() as usize) % bytes.len();
            match rng.below(3) {
                0 => bytes[at] = NOISE[(rng.next() as usize) % NOISE.len()],
                1 => bytes.truncate(at),
                _ => bytes.insert(at, NOISE[(rng.next() as usize) % NOISE.len()]),
            }
            if bytes.is_empty() {
                break;
            }
        }
        // Only compare on inputs that are valid UTF-8; `from_str` on both sides
        // takes `&str`, and byte-level UTF-8 handling is covered separately.
        if let Ok(text) = std::str::from_utf8(&bytes) {
            agree_parse(text);
        }
    }
}

/// Raw byte strings, including invalid UTF-8, through `from_slice`.
#[test]
fn arbitrary_bytes_get_the_same_verdict() {
    let mut rng = Rng(0xDEAD_BEEF_1234_5678);
    for _ in 0..40_000 {
        let len = rng.below(24) as usize;
        let bytes: Vec<u8> = (0..len)
            .map(|_| {
                const ALPH: &[u8] = b"{}[],:\"\\ \n0129e+-.ab\xff\xc3\xa9\x01ntrufalsl";
                ALPH[(rng.next() as usize) % ALPH.len()]
            })
            .collect();
        let ours = aterm_json::from_slice::<aterm_json::Value>(&bytes);
        let theirs = serde_json::from_slice::<serde_json::Value>(&bytes);
        assert_eq!(
            ours.is_ok(),
            theirs.is_ok(),
            "verdicts differ for {:?}: ours={:?} oracle={:?}",
            String::from_utf8_lossy(&bytes),
            ours.as_ref().err().map(ToString::to_string),
            theirs.as_ref().err().map(ToString::to_string),
        );
        if let (Ok(a), Ok(b)) = (&ours, &theirs) {
            assert_eq!(
                aterm_json::to_string(a).unwrap(),
                serde_json::to_string(b).unwrap(),
                "trees differ for {:?}",
                String::from_utf8_lossy(&bytes)
            );
        }
    }
}

/// The named edge cases: every one of these is a decision the module docs
/// record, checked against the reference rather than against the docs.
#[test]
fn named_edge_cases_match_the_oracle() {
    for input in [
        // numbers
        "0",
        "-0",
        "0.0",
        "-0.0",
        "1",
        "-1",
        "1e0",
        "1E0",
        "1e+2",
        "1e-2",
        "9223372036854775807",
        "-9223372036854775808",
        "18446744073709551615",
        "18446744073709551616",
        "-9223372036854775809",
        "123456789012345678901234567890",
        "1.7976931348623157e308",
        "1e309",
        "1e-400",
        "5e-324",
        "0.1",
        "1.5",
        "-1.5",
        // malformed numbers
        "01",
        "-01",
        "1.",
        ".5",
        "1e",
        "1e+",
        "+1",
        "-",
        "00",
        "0x1",
        "1_000",
        "Infinity",
        "NaN",
        "1.0e",
        "--1",
        "1..2",
        // literals
        "null",
        "true",
        "false",
        "nul",
        "tru",
        "fals",
        "NULL",
        "True",
        // strings
        r#""""#,
        r#""a""#,
        r#""A""#,
        r#""é""#,
        r#""😀""#,
        r#""\ud83d""#,
        r#""\ude00""#,
        r#""\ud83dA""#,
        r#""\x41""#,
        r#""\""#,
        r#""a"#,
        "\"\n\"",
        "\"\t\"",
        "\"\u{1}\"",
        r#""\/""#,
        r#""\b\f\n\r\t""#,
        r#""😀""#,
        // structure
        "[]",
        "{}",
        "[1]",
        "[1,]",
        "{,}",
        r#"{"a":1}"#,
        r#"{"a":1,}"#,
        r#"{"a":}"#,
        r#"{"a"}"#,
        r#"{a:1}"#,
        r#"{'a':1}"#,
        "[1 2]",
        r#"{"a":1 "b":2}"#,
        r#"{"a":1,"a":2}"#,
        r#"{"a":1,"a":2,"a":3}"#,
        // whitespace and trailing data
        "  1  ",
        "\n\t{}\r\n",
        "1 2",
        "{} {}",
        "[]x",
        "",
        "   ",
        "\u{feff}1",
        // nesting
        "[[[[[[[[[[1]]]]]]]]]]",
    ] {
        agree_parse(input);
    }
}

/// The recursion bound, at the exact boundary `serde_json` draws it.
#[test]
fn the_recursion_limit_matches_the_oracle() {
    for depth in [1usize, 100, 127, 128, 129, 200, 5_000] {
        for (open, close) in [('[', ']'), ('{', '}')] {
            let mut doc = String::new();
            for _ in 0..depth {
                doc.push(open);
                if open == '{' {
                    doc.push_str("\"k\":");
                }
            }
            doc.push('1');
            for _ in 0..depth {
                doc.push(close);
            }
            let ours = aterm_json::from_str::<aterm_json::Value>(&doc);
            let theirs = serde_json::from_str::<serde_json::Value>(&doc);
            assert_eq!(
                ours.is_ok(),
                theirs.is_ok(),
                "depth {depth} of {open}: ours={:?} oracle={:?}",
                ours.as_ref().err().map(ToString::to_string),
                theirs.as_ref().err().map(ToString::to_string),
            );
        }
    }
}

/// Floats: `serde_json` writes the shortest decimal that round-trips, laid out
/// fixed or scientific by exponent. Byte-identity over a wide sample is the
/// only way to know the LAYOUT rule was reproduced rather than approximated.
#[test]
fn float_formatting_is_byte_identical() {
    let mut sample: Vec<f64> = vec![
        0.0,
        -0.0,
        1.0,
        -1.0,
        0.1,
        0.5,
        100.0,
        1e15,
        1e16,
        1e17,
        1e21,
        1e22,
        1.5e-5,
        1e-5,
        1e-6,
        1e-7,
        1e300,
        1e-300,
        1.234_567_890_123e10,
        3.0e-1,
        2.5,
        1234.5678,
        f64::MIN_POSITIVE,
        5e-324,
        f64::MAX,
        f64::MIN,
        0.3,
        1e-4,
        1e-3,
        1e18,
        1e19,
        1e20,
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        123_456_789_012_345_680_000.0,
        9_007_199_254_740_993.0,
        0.000_001,
        0.000_01,
    ];
    let mut rng = Rng(0x0BAD_C0DE_F00D_1234);
    for _ in 0..50_000 {
        // Uniform over the bit pattern, so every exponent range is reached.
        let bits = (u64::from(rng.next()) << 32) | u64::from(rng.next());
        let value = f64::from_bits(bits);
        if value.is_finite() {
            sample.push(value);
        }
        // And a second sample biased to "human" magnitudes.
        let scale = 10f64.powi((rng.below(40) as i32) - 20);
        sample.push(f64::from(rng.next()) * scale);
    }
    eprintln!("float corpus: {} values", sample.len());
    for value in sample {
        let ours = aterm_json::to_string(&value).expect("serialize");
        let theirs = serde_json::to_string(&value).expect("serialize");
        assert_eq!(ours, theirs, "f64 {value:?} ({:#x})", value.to_bits());
        // And the text must read back as the same bits.
        if value.is_finite() {
            let back: f64 = aterm_json::from_str(&ours).expect("round-trip");
            assert_eq!(back.to_bits(), value.to_bits(), "round-trip {value:?}");
        }
    }
}

/// `f32` is formatted through the f32 shortest form, not widened first.
#[test]
fn f32_formatting_is_byte_identical() {
    let mut rng = Rng(0x1234_ABCD_5678_EF01);
    let mut sample: Vec<f32> = vec![0.0, -0.0, 0.1, 1.0, 3.4e38, 1.2e-38, 16_777_217.0];
    for _ in 0..30_000 {
        let value = f32::from_bits(rng.next());
        if value.is_finite() {
            sample.push(value);
        }
    }
    for value in sample {
        assert_eq!(
            aterm_json::to_string(&value).unwrap(),
            serde_json::to_string(&value).unwrap(),
            "f32 {value:?}"
        );
    }
}

/// Integers across every width and both signs.
#[test]
fn integer_formatting_is_byte_identical() {
    macro_rules! check {
        ($($v:expr),* $(,)?) => { $(
            assert_eq!(
                aterm_json::to_string(&$v).unwrap(),
                serde_json::to_string(&$v).unwrap(),
                "{}", stringify!($v)
            );
        )* };
    }
    check!(
        0u8,
        u8::MAX,
        0i8,
        i8::MIN,
        i8::MAX,
        u16::MAX,
        i16::MIN,
        u32::MAX,
        i32::MIN,
        u64::MAX,
        i64::MIN,
        i64::MAX,
        0u64,
        0i64,
        u128::MAX,
        i128::MIN,
        i128::MAX,
    );
}

/// String escaping: only ASCII controls, and only in the forms `serde_json`
/// picks.
#[test]
fn string_escaping_is_byte_identical() {
    let mut cases: Vec<String> = Vec::new();
    for b in 0u32..=0x1FF {
        if let Some(ch) = char::from_u32(b) {
            cases.push(ch.to_string());
            cases.push(format!("a{ch}b"));
        }
    }
    cases.extend([
        String::new(),
        "\"".into(),
        "\\".into(),
        "/".into(),
        "é世🙂".into(),
        "\u{7f}".into(),
        "\u{2028}\u{2029}".into(),
        "line\nbreak\ttab\r\n".into(),
    ]);
    for case in cases {
        assert_eq!(
            aterm_json::to_string(&case).unwrap(),
            serde_json::to_string(&case).unwrap(),
            "{case:?}"
        );
        agree_parse(&serde_json::to_string(&case).unwrap());
    }
}

// ── typed models ───────────────────────────────────────────────────────────

/// A model shaped like the ones this tree actually deserializes: renamed
/// variants, container and field `default`, `skip_serializing_if`, `Option`,
/// nesting, a map, and an enum.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Phase {
    Pending,
    Running,
    Done,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
struct Row {
    phase: Option<Phase>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    bytes: u64,
    ratio: f64,
    flag: bool,
    tags: Vec<String>,
    extra: std::collections::BTreeMap<String, i64>,
    /// A FIXED-SIZE array, and the reason this field is here: `serde`'s visitor
    /// for `[T; N]` takes exactly N elements and returns WITHOUT draining to
    /// the closing bracket, unlike a `Vec`'s. A deserializer whose sequence
    /// access consumed the `]` itself works for one and leaves a stray bracket
    /// for the other — which is precisely the defect the differential run
    /// against `aterm-core`'s `CheckpointMeta` (a `[u8; 8]` keyboard stack)
    /// found in this crate.
    stack: [u8; 8],
    /// A tuple, which has the same early-stopping visitor.
    pair: (u8, String),
    /// A newtype over a fixed array, nested one level deeper.
    nested_fixed: Wrapped,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
struct Wrapped([u16; 3]);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
struct Doc {
    v: u32,
    name: String,
    rows: Vec<Row>,
    nested: Option<Box<Doc>>,
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct Strict {
    a: u64,
    b: String,
}

#[test]
fn typed_models_serialize_to_identical_bytes() {
    let mut rng = Rng(0x5DEE_CE66_D1E5_C0DE);
    for _ in 0..2_000 {
        let row = |rng: &mut Rng| Row {
            phase: match rng.below(4) {
                0 => None,
                1 => Some(Phase::Pending),
                2 => Some(Phase::Running),
                _ => Some(Phase::Done),
            },
            detail: (rng.next().is_multiple_of(2)).then(|| format!("d{}", rng.next())),
            bytes: u64::from(rng.next()),
            ratio: f64::from(rng.next()) / 1024.0,
            flag: rng.next().is_multiple_of(2),
            tags: (0..rng.below(4)).map(|i| format!("t{i}")).collect(),
            extra: (0..rng.below(3))
                .map(|i| (format!("e{i}"), i64::from(rng.next()) - 1000))
                .collect(),
            stack: std::array::from_fn(|_| (rng.next() & 0xFF) as u8),
            pair: ((rng.next() & 0xFF) as u8, format!("p{}", rng.below(9))),
            nested_fixed: Wrapped(std::array::from_fn(|_| (rng.next() & 0xFFFF) as u16)),
        };
        let doc = Doc {
            v: rng.below(5),
            name: format!("n{}", rng.next()),
            rows: (0..rng.below(4)).map(|_| row(&mut rng)).collect(),
            nested: (rng.next().is_multiple_of(3)).then(|| Box::new(Doc::default())),
        };
        let ours = aterm_json::to_string(&doc).expect("ours");
        let theirs = serde_json::to_string(&doc).expect("theirs");
        assert_eq!(ours, theirs, "typed serialization differs");

        // Ours parses the ORACLE's output back to the original value EXACTLY.
        // The reverse is asserted only up to the oracle's own float-parsing
        // slack (see `our_float_parsing_is_correctly_rounded`), so it is
        // checked by re-serializing rather than by equality.
        let a: Doc = aterm_json::from_str(&theirs).expect("ours parses theirs");
        assert_eq!(a, doc);
        let b: Doc = serde_json::from_str(&ours).expect("theirs parses ours");
        assert_eq!(
            serde_json::to_string(&b).unwrap().len(),
            theirs.len(),
            "oracle's re-parse changed the document's shape"
        );
    }
}

#[test]
fn typed_deserialization_agrees_on_verdict_and_value() {
    for input in [
        r#"{"v":1,"name":"x","rows":[],"nested":null}"#,
        r#"{}"#,
        r#"{"v":1}"#,
        r#"{"unknown":1}"#,
        r#"{"v":"not a number"}"#,
        r#"{"rows":[{"phase":"running","bytes":3}]}"#,
        r#"{"rows":[{"phase":"RUNNING"}]}"#,
        r#"{"rows":[{"phase":null}]}"#,
        r#"{"v":1,"v":2}"#,
        r#"{"nested":{"v":9}}"#,
        r#"{"v":1.5}"#,
        r#"{"rows":[{"ratio":1}]}"#,
        r#"{"rows":[{"extra":{"a":1,"b":-2}}]}"#,
        // Fixed-size arrays and tuples: exact, short, long, and wrong-typed.
        r#"{"rows":[{"stack":[1,2,3,4,5,6,7,8]}]}"#,
        r#"{"rows":[{"stack":[1,2,3]}]}"#,
        r#"{"rows":[{"stack":[1,2,3,4,5,6,7,8,9]}]}"#,
        r#"{"rows":[{"stack":[1,2,3,4,5,6,7,8],"bytes":1}]}"#,
        r#"{"rows":[{"stack":"nope"}]}"#,
        r#"{"rows":[{"pair":[7,"x"]}]}"#,
        r#"{"rows":[{"pair":[7]}]}"#,
        r#"{"rows":[{"pair":[7,"x","extra"]}]}"#,
        r#"{"rows":[{"nested_fixed":[1,2,3]}]}"#,
        r#"{"rows":[{"nested_fixed":[1,2,3,4]}]}"#,
        // NEGATIVE ZERO into a typed integer field. `serde_json` refuses it —
        // its typed integer path runs the same `parse_number` its untyped one
        // does, and that turns `-0` into the float `-0.0` on both. This reader
        // used to accept it as plain `0`, which is an acceptance-set widening
        // on the path that reads the GitHub Releases discovery reply.
        r#"{"v":-0}"#,
        r#"{"v":-0.0}"#,
        r#"{"v":0}"#,
        r#"{"rows":[{"bytes":-0}]}"#,
        r#"{"rows":[{"ratio":-0}]}"#,
    ] {
        let ours = aterm_json::from_str::<Doc>(input);
        let theirs = serde_json::from_str::<Doc>(input);
        assert_eq!(
            ours.is_ok(),
            theirs.is_ok(),
            "verdicts differ for {input}: ours={:?} oracle={:?}",
            ours.as_ref().err().map(ToString::to_string),
            theirs.as_ref().err().map(ToString::to_string),
        );
        if let (Ok(a), Ok(b)) = (&ours, &theirs) {
            assert_eq!(a, b, "values differ for {input}");
        }
    }
}

/// `deny_unknown_fields` has to actually deny, which it only does if the
/// deserializer drives the derived visitor the way `serde_json` does.
/// Every integer WIDTH against the same literals, at the top level rather than
/// in a struct field — the typed entry points `deserialize_u8` … `u128` each
/// dispatch separately, and `-0` and the 128-bit range are exactly where they
/// used to diverge from the reference.
#[test]
fn scalar_integer_targets_agree_with_the_oracle() {
    macro_rules! check {
        ($ty:ty, $input:expr) => {{
            let ours = aterm_json::from_str::<$ty>($input);
            let theirs = serde_json::from_str::<$ty>($input);
            assert_eq!(
                ours.is_ok(),
                theirs.is_ok(),
                "verdicts differ for {} as {}: ours={:?} oracle={:?}",
                $input,
                stringify!($ty),
                ours.as_ref().err().map(ToString::to_string),
                theirs.as_ref().err().map(ToString::to_string),
            );
            if let (Ok(a), Ok(b)) = (&ours, &theirs) {
                assert_eq!(a, b, "values differ for {} as {}", $input, stringify!($ty));
            }
        }};
    }
    let wide_u = u128::MAX.to_string();
    let wide_i = i128::MIN.to_string();
    let past_u64 = "18446744073709551616";
    let past_u128 = "340282366920938463463374607431768211456";
    for input in [
        "-0",
        "-0.0",
        "0",
        "1",
        "-1",
        "1.5",
        "1e3",
        "0e0",
        wide_u.as_str(),
        wide_i.as_str(),
        past_u64,
        past_u128,
        "9223372036854775807",
        "-9223372036854775808",
        "18446744073709551615",
    ] {
        check!(u8, input);
        check!(i8, input);
        check!(u16, input);
        check!(i16, input);
        check!(u32, input);
        check!(i32, input);
        check!(u64, input);
        check!(i64, input);
        check!(usize, input);
        check!(isize, input);
        check!(u128, input);
        check!(i128, input);
        check!(f64, input);
        check!(aterm_json::Value, input);
    }
}

/// The 128-bit round trip, which `integer_formatting_is_byte_identical` only
/// ever checked the WRITE half of: this crate emits `u128::MAX` exactly, so it
/// has to read it back exactly, and `to_value` of it must FAIL rather than hand
/// back a silently rounded float.
#[test]
fn wide_integers_round_trip_or_fail_like_the_oracle() {
    for value in [u128::MAX, u128::from(u64::MAX) + 1, 0, 1] {
        let ours = aterm_json::to_string(&value).expect("to_string");
        let theirs = serde_json::to_string(&value).expect("to_string");
        assert_eq!(ours, theirs);
        assert_eq!(
            aterm_json::from_str::<u128>(&ours).expect("round trip"),
            serde_json::from_str::<u128>(&theirs).expect("round trip"),
        );
    }
    for value in [i128::MIN, i128::MAX, i128::from(i64::MIN) - 1, -1, 0] {
        let ours = aterm_json::to_string(&value).expect("to_string");
        assert_eq!(ours, serde_json::to_string(&value).expect("to_string"));
        assert_eq!(
            aterm_json::from_str::<i128>(&ours).expect("round trip"),
            value
        );
    }
    // `to_value` fails CLOSED on a value `Value::Number` cannot hold, the way
    // the reference does, rather than substituting a rounded float.
    for (ours, theirs) in [
        (
            aterm_json::to_value(u128::MAX).is_ok(),
            serde_json::to_value(u128::MAX).is_ok(),
        ),
        (
            aterm_json::to_value(i128::MIN).is_ok(),
            serde_json::to_value(i128::MIN).is_ok(),
        ),
        (
            aterm_json::to_value(u64::MAX).is_ok(),
            serde_json::to_value(u64::MAX).is_ok(),
        ),
        (
            aterm_json::to_value(i64::MIN).is_ok(),
            serde_json::to_value(i64::MIN).is_ok(),
        ),
    ] {
        assert_eq!(ours, theirs, "to_value verdicts differ");
    }
}

/// A value that CANNOT serialize must be refused by both, not turned into
/// `null` by one of them.
///
/// `json!`'s expression arm used to be `unwrap_or(Value::Null)`, so a map with
/// a non-string key became a document quietly carrying `null` where the field
/// should have been — in the LLM request-body builder and the control-payload
/// builder. The reference panics; so does this now, which is why the macro
/// itself is exercised in a `catch_unwind` rather than by calling it directly.
#[test]
fn a_value_that_cannot_serialize_is_refused_by_both() {
    use std::collections::BTreeMap;
    let bad: BTreeMap<(u8, u8), u8> = [((1, 2), 3)].into_iter().collect();
    assert!(aterm_json::to_value(&bad).is_err());
    assert!(serde_json::to_value(&bad).is_err());
    assert!(aterm_json::to_string(&bad).is_err());
    assert!(serde_json::to_string(&bad).is_err());

    let ours = std::panic::catch_unwind(|| {
        let bad: BTreeMap<(u8, u8), u8> = [((1, 2), 3)].into_iter().collect();
        aterm_json::json!({ "field": bad })
    });
    let theirs = std::panic::catch_unwind(|| {
        let bad: BTreeMap<(u8, u8), u8> = [((1, 2), 3)].into_iter().collect();
        serde_json::json!({ "field": bad })
    });
    assert!(ours.is_err(), "json! must not substitute null");
    assert!(theirs.is_err(), "the reference panics here");
}

#[test]
fn deny_unknown_fields_agrees_with_the_oracle() {
    for input in [
        r#"{"a":1,"b":"x"}"#,
        r#"{"a":1,"b":"x","c":2}"#,
        r#"{"b":"x"}"#,
        r#"{"a":1,"b":"x","a":2}"#,
    ] {
        let ours = aterm_json::from_str::<Strict>(input);
        let theirs = serde_json::from_str::<Strict>(input);
        assert_eq!(
            ours.is_ok(),
            theirs.is_ok(),
            "verdicts differ for {input}: ours={:?} oracle={:?}",
            ours.as_ref().err().map(ToString::to_string),
            theirs.as_ref().err().map(ToString::to_string),
        );
    }
}

/// The `json!` macro must build the same document `serde_json::json!` builds —
/// nesting, arrays of objects, expression values, and a `Value` spliced in.
#[test]
fn the_json_macro_matches_the_oracle() {
    let model = "qwen3.5";
    let system = "be brief";
    let context = String::from("ctx");
    let keep_alive_ours = aterm_json::json!(-1);
    let keep_alive_theirs = serde_json::json!(-1);

    let ours = aterm_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": context}
        ],
        "stream": false,
        "think": false,
        "keep_alive": keep_alive_ours,
        "format": {
            "type": "object",
            "properties": {"description": {"type": "string"}},
            "required": ["description"],
            "additionalProperties": false
        },
        "options": {"temperature": 0, "num_predict": 64, "num_ctx": 4096}
    });
    let theirs = serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": "ctx"}
        ],
        "stream": false,
        "think": false,
        "keep_alive": keep_alive_theirs,
        "format": {
            "type": "object",
            "properties": {"description": {"type": "string"}},
            "required": ["description"],
            "additionalProperties": false
        },
        "options": {"temperature": 0, "num_predict": 64, "num_ctx": 4096}
    });
    assert_eq!(
        aterm_json::to_string(&ours).unwrap(),
        serde_json::to_string(&theirs).unwrap()
    );

    // Empty containers, a trailing comma, null, and a computed value.
    let index = 3usize;
    assert_eq!(
        aterm_json::to_string(&aterm_json::json!({
            "empty_obj": {},
            "empty_arr": [],
            "null": null,
            "computed": format!("frame-{index}.png"),
        }))
        .unwrap(),
        serde_json::to_string(&serde_json::json!({
            "empty_obj": {},
            "empty_arr": [],
            "null": null,
            "computed": format!("frame-{index}.png"),
        }))
        .unwrap()
    );

    // Bare values.
    assert_eq!(
        aterm_json::to_string(&aterm_json::json!("10m")).unwrap(),
        serde_json::to_string(&serde_json::json!("10m")).unwrap()
    );
    assert_eq!(
        aterm_json::to_string(&aterm_json::json!(null)).unwrap(),
        serde_json::to_string(&serde_json::json!(null)).unwrap()
    );
    assert_eq!(
        aterm_json::to_string(&aterm_json::json!([1, "two", true, null, {"a": 1}])).unwrap(),
        serde_json::to_string(&serde_json::json!([1, "two", true, null, {"a": 1}])).unwrap()
    );
}

/// `to_value` / `from_value` round-trip a typed model through the untyped tree.
#[test]
fn to_value_and_from_value_match_the_oracle() {
    let doc = Doc {
        v: 2,
        name: "x".into(),
        rows: vec![Row {
            phase: Some(Phase::Done),
            detail: Some("d".into()),
            bytes: 7,
            ratio: 0.5,
            flag: true,
            tags: vec!["a".into()],
            extra: [("k".to_string(), -1i64)].into_iter().collect(),
            stack: [1, 2, 3, 4, 5, 6, 7, 8],
            pair: (9, "p".into()),
            nested_fixed: Wrapped([10, 11, 12]),
        }],
        nested: None,
    };
    let ours = aterm_json::to_value(&doc).expect("to_value");
    let theirs = serde_json::to_value(&doc).expect("to_value");
    assert_eq!(
        aterm_json::to_string(&ours).unwrap(),
        serde_json::to_string(&theirs).unwrap()
    );
    let back: Doc = aterm_json::from_value(ours).expect("from_value");
    assert_eq!(back, doc);
}

/// `Display` on a `Value` is a compact document — the behaviour every
/// `format!("{data}")` interpolation in this tree relies on.
#[test]
fn value_display_matches_the_oracle() {
    let ours = aterm_json::json!({"b": 1, "a": [true, null, "x"]});
    let theirs = serde_json::json!({"b": 1, "a": [true, null, "x"]});
    assert_eq!(ours.to_string(), theirs.to_string());
}

/// The repository's own JSON, wherever it lives.
#[test]
fn repository_json_corpus_matches_the_oracle() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root")
        .to_path_buf();
    let mut stack = vec![root];
    let mut files = 0usize;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                if path
                    .file_name()
                    .is_some_and(|n| n == "target" || n == ".git" || n == "target-tippy")
                {
                    continue;
                }
                stack.push(path);
            } else if meta.is_file()
                && meta.len() <= 4 * 1024 * 1024
                && path.extension().is_some_and(|e| e == "json")
            {
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                agree_parse(&text);
                files += 1;
            }
        }
    }
    eprintln!("json corpus: {files} files");
}

/// THE ONE DOCUMENTED DIVERGENCE, and it is the oracle that gives ground.
///
/// `serde_json` parses floats through a fast path that multiplies a significand
/// by a power of ten in binary floating point, and it is off by one ULP for
/// inputs as ordinary as `935092.5986328125` — and by more than that for a long
/// significand with an exponent (`959695331.2032990304e-19` lands about
/// fourteen ULPs out). This crate defers to Rust's `dec2flt`, which is
/// correctly rounded. So the property asserted here is not "we match" but the
/// stronger one: OUR parse is exactly the nearest double to the decimal
/// written, and the oracle is merely close.
///
/// Nothing in aterm observes the difference — no JSON model in this tree has a
/// float field, and every untyped read is `as_str` or `as_u64` — but a
/// replacement that reproduced a rounding bug on purpose would be the wrong
/// kind of faithful.
#[test]
fn our_float_parsing_is_correctly_rounded() {
    let mut rng = Rng(0xF10A_7C0D_E123_4567);
    let mut cases: Vec<String> = vec![
        "935092.5986328125".into(),
        "202449325.1295808655".into(),
        "0.1".into(),
        "1e-300".into(),
        "1.7976931348623157e308".into(),
        "5e-324".into(),
    ];
    for _ in 0..20_000 {
        cases.push(format!("{}.{}", rng.next(), rng.next()));
        cases.push(format!("{}.{}e{}", rng.next(), rng.next(), rng.below(40)));
        cases.push(format!("{}.{}e-{}", rng.next(), rng.next(), rng.below(40)));
    }
    for text in cases {
        let Ok(exact) = text.parse::<f64>() else {
            continue;
        };
        let ours: f64 = aterm_json::from_str(&text).expect("ours parses");
        assert_eq!(
            ours.to_bits(),
            exact.to_bits(),
            "{text}: ours is not the nearest double"
        );
        let theirs: f64 = serde_json::from_str(&text).expect("oracle parses");
        assert!(
            floats_agree(ours, theirs),
            "{text}: ours {ours:?} vs oracle {theirs:?} are not even close"
        );
    }
}

/// JSON Pointer (RFC 6901): the LLM transport reaches into a provider's reply
/// with `/message/content` and `/choices/0/message/content`, so the resolution
/// rules — including the ones that must FAIL — have to match.
#[test]
fn json_pointer_matches_the_oracle() {
    let text = r#"{
        "message": {"content": "hi"},
        "choices": [{"message": {"content": "first"}}, {"message": {"content": "second"}}],
        "a/b": 1,
        "a~b": 2,
        "": 3,
        "list": [10, 20, 30]
    }"#;
    let ours: aterm_json::Value = aterm_json::from_str(text).unwrap();
    let theirs: serde_json::Value = serde_json::from_str(text).unwrap();
    for pointer in [
        "",
        "/message/content",
        "/choices/0/message/content",
        "/choices/1/message/content",
        "/choices/2/message/content",
        "/list/0",
        "/list/2",
        "/list/3",
        "/list/00",
        "/list/+0",
        "/list/-",
        "/list/x",
        "/a~1b",
        "/a~0b",
        "/",
        "/missing",
        "/message/content/deeper",
        "message/content",
        "/choices/0/message",
    ] {
        let a = ours.pointer(pointer);
        let b = theirs.pointer(pointer);
        assert_eq!(
            a.is_some(),
            b.is_some(),
            "pointer {pointer:?}: ours={a:?} oracle={b:?}"
        );
        if let (Some(a), Some(b)) = (a, b) {
            assert!(values_agree(a, b), "pointer {pointer:?}");
        }
    }
}

/// `value["key"]` is total: a miss is `Null`, not a panic. Assertions in this
/// tree read a metrics reply that way.
#[test]
fn index_sugar_matches_the_oracle() {
    let text = r#"{"rows": 24, "cols": null, "list": [1, 2]}"#;
    let ours: aterm_json::Value = aterm_json::from_str(text).unwrap();
    let theirs: serde_json::Value = serde_json::from_str(text).unwrap();
    for key in ["rows", "cols", "missing", ""] {
        assert_eq!(ours[key].is_null(), theirs[key].is_null(), "index {key:?}");
        assert!(values_agree(&ours[key], &theirs[key]), "index {key:?}");
    }
    for i in 0..4 {
        assert!(
            values_agree(&ours["list"][i], &theirs["list"][i]),
            "index [{i}]"
        );
    }
    // Indexing a non-object by a key is a miss, not a panic.
    assert!(ours["rows"]["nope"].is_null());
    assert!(theirs["rows"]["nope"].is_null());
}
