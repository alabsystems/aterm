// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm_json`: verdicts and bytes.
//!
//! Serialization output matters byte for byte (some of this JSON is hashed into
//! a checkpoint fingerprint, some is a request body), and a parser that is
//! merely more permissive hands the program a document the reference would
//! have rejected. The sampled sweeps below are pinned by digest; each was
//! recorded when every verdict and every byte matched `serde_json`.

use serde::{Deserialize, Serialize};

/// FNV-1a over a sweep's outputs, so a whole sweep is pinned by one value.
struct Golden(u64);

impl Golden {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes.iter().chain(&[0xff]) {
            self.0 ^= u64::from(b);
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
    }

    /// A parse: its verdict, and on success the compact re-serialization.
    fn parsed(&mut self, result: &Result<aterm_json::Value, aterm_json::Error>) {
        match result {
            Ok(v) => self.feed(aterm_json::to_string(v).expect("serialize").as_bytes()),
            Err(_) => self.feed(b"ERR"),
        }
    }

    /// A typed read: its verdict, and on success the value's `Debug`.
    fn typed<T: std::fmt::Debug>(&mut self, result: &Result<T, aterm_json::Error>) {
        match result {
            Ok(v) => self.feed(format!("{v:?}").as_bytes()),
            Err(_) => self.feed(b"ERR"),
        }
    }

    fn check(&self, pinned: &str) {
        assert_eq!(
            format!("{:016x}", self.0),
            pinned,
            "the pinned sweep changed"
        );
    }
}

/// Parse, and hold a successful parse to a lossless re-serialization.
fn parse(input: &str) -> Result<aterm_json::Value, aterm_json::Error> {
    let result = aterm_json::from_str::<aterm_json::Value>(input);
    if let Ok(v) = &result {
        let text = aterm_json::to_string(v).expect("serialize");
        let back: aterm_json::Value = aterm_json::from_str(&text).expect("re-parse");
        assert_eq!(&back, v, "re-serialization of {input:?} is lossy");
    }
    result
}

#[test]
fn ignored_strings_validate_the_same_bytes_as_materialized_values() {
    use serde::de::IgnoredAny;

    let mut cases = vec![
        br#""ordinary UTF-8""#.to_vec(),
        "\"日本語 👩🏽‍💻\"".as_bytes().to_vec(),
        br#""\"\\\/\b\f\n\r\t\u0000\uD83D\uDE80""#.to_vec(),
        br#""\uD800""#.to_vec(),
        br#""\uDC00""#.to_vec(),
        br#""\uD800\u0041""#.to_vec(),
        br#""\uD800\x0000""#.to_vec(),
        br#""\u0xx0""#.to_vec(),
        br#""\q""#.to_vec(),
        b"\"unterminated\\".to_vec(),
        b"\"a\nb\"".to_vec(),
        b"\"\xff\"".to_vec(),
        b"\"\xff\\n\"".to_vec(),
        b"\"\\n\xff\"".to_vec(),
        b"\"\xc3\\n\xa9\"".to_vec(),
    ];
    let mut rng = Rng(0x1_6202_0928);
    for _ in 0..1000 {
        let mut text = String::new();
        random_json(&mut rng, 0, &mut text);
        cases.push(text.into_bytes());
    }
    for raw in cases {
        // Test values, nested values, and escaped map keys: every discarded
        // string path must reject malformed UTF-8 and escapes alike.
        let mut nested = b"{\"ignored\":[".to_vec();
        nested.extend_from_slice(&raw);
        nested.extend_from_slice(b"]}");
        let mut key = b"{".to_vec();
        key.extend_from_slice(&raw);
        key.extend_from_slice(b":0}");
        for bytes in [raw, nested, key] {
            let full = aterm_json::from_slice::<aterm_json::Value>(&bytes);
            let skipped = aterm_json::from_slice::<IgnoredAny>(&bytes);
            assert_eq!(full.is_ok(), skipped.is_ok(), "{bytes:?}");
        }
    }
}

// ── helpers ────────────────────────────────────────────────────────────────

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
fn generated_documents_parse_and_reserialize() {
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    let mut golden = Golden::new();
    for _ in 0..20_000 {
        let mut doc = String::new();
        random_json(&mut rng, 0, &mut doc);
        let result = parse(&doc);
        assert!(result.is_ok(), "a generated document was refused: {doc}");
        golden.parsed(&result);
    }
    golden.check(GENERATED);
}

/// Mutated valid JSON: where a too-permissive parser shows itself.
#[test]
fn mutated_documents_get_their_pinned_verdicts() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut golden = Golden::new();
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
        if let Ok(text) = std::str::from_utf8(&bytes) {
            golden.parsed(&parse(text));
        }
    }
    golden.check(MUTATED);
}

/// Raw byte strings, including invalid UTF-8, through `from_slice`.
#[test]
fn arbitrary_bytes_get_their_pinned_verdicts() {
    let mut rng = Rng(0xDEAD_BEEF_1234_5678);
    let mut golden = Golden::new();
    for _ in 0..40_000 {
        let len = rng.below(24) as usize;
        let bytes: Vec<u8> = (0..len)
            .map(|_| {
                const ALPH: &[u8] = b"{}[],:\"\\ \n0129e+-.ab\xff\xc3\xa9\x01ntrufalsl";
                ALPH[(rng.next() as usize) % ALPH.len()]
            })
            .collect();
        golden.parsed(&aterm_json::from_slice::<aterm_json::Value>(&bytes));
    }
    golden.check(ARBITRARY);
}

/// The named edge cases: every one of these is a decision the module docs
/// record.
#[test]
fn named_edge_cases_get_their_pinned_verdicts() {
    let mut golden = Golden::new();
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
        golden.feed(input.as_bytes());
        golden.parsed(&parse(input));
    }
    golden.check(NAMED);
}

/// The recursion bound: 128 levels.
#[test]
fn the_recursion_limit() {
    let mut golden = Golden::new();
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
            let ok = aterm_json::from_str::<aterm_json::Value>(&doc).is_ok();
            golden.feed(&[u8::from(ok)]);
        }
    }
    golden.check(RECURSION);
}

/// Floats: the shortest decimal that round-trips, laid out fixed or scientific
/// by exponent. Every value must also read back as the same bits.
#[test]
fn float_formatting_is_pinned_and_round_trips() {
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
    let mut golden = Golden::new();
    for value in sample {
        let ours = aterm_json::to_string(&value).expect("serialize");
        golden.feed(ours.as_bytes());
        if value.is_finite() {
            let back: f64 = aterm_json::from_str(&ours).expect("round-trip");
            assert_eq!(back.to_bits(), value.to_bits(), "round-trip {value:?}");
        }
    }
    golden.check(FLOATS);
}

/// `f32` is formatted through the f32 shortest form, not widened first.
#[test]
fn f32_formatting_is_pinned() {
    let mut rng = Rng(0x1234_ABCD_5678_EF01);
    let mut sample: Vec<f32> = vec![0.0, -0.0, 0.1, 1.0, 3.4e38, 1.2e-38, 16_777_217.0];
    for _ in 0..30_000 {
        let value = f32::from_bits(rng.next());
        if value.is_finite() {
            sample.push(value);
        }
    }
    let mut golden = Golden::new();
    for value in sample {
        golden.feed(aterm_json::to_string(&value).unwrap().as_bytes());
    }
    golden.check(F32S);
}

/// The digits are the SHORTEST that read back, at an exact power of two too.
///
/// A power of two has a lopsided rounding interval — the next double down is
/// half as far away as the next one up — so the decimal of a given length
/// closest to it can miss the interval while a farther one of the same length
/// lands inside. `ryu` (what `serde_json` writes with), Python's `repr` and
/// Rust's own `{:e}` all print `2^-24` in 16 digits; walking the correctly
/// rounded precision up wrote 17. The literals below are Python's `repr`; the
/// sweep holds every power of two of both widths to the digit count of Rust's
/// shortest form.
#[test]
fn a_power_of_two_gets_the_shortest_digits() {
    assert_eq!(
        aterm_json::to_string(&2f64.powi(-24)).unwrap(),
        "5.960464477539063e-8"
    );
    assert_eq!(
        aterm_json::to_string(&2f64.powi(89)).unwrap(),
        "6.189700196426902e+26"
    );
    assert_eq!(
        aterm_json::to_string(&2f64.powi(976)).unwrap(),
        "6.386688990511104e+293"
    );
    assert_eq!(
        aterm_json::to_string(&2f32.powi(-96)).unwrap(),
        "1.2621775e-29"
    );

    fn digit_count(text: &str) -> usize {
        let mantissa = text.split(['e', 'E']).next().unwrap_or(text);
        let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
        digits.trim_start_matches('0').trim_end_matches('0').len()
    }
    for biased in 1..0x7ffu64 {
        let value = f64::from_bits(biased << 52);
        let ours = aterm_json::to_string(&value).unwrap();
        assert_eq!(ours.parse::<f64>(), Ok(value), "{ours} must read back");
        assert_eq!(
            digit_count(&ours),
            digit_count(&format!("{value:e}")),
            "2^{}: {ours} is not the shortest form of {value:e}",
            biased as i64 - 1023
        );
    }
    for biased in 1..0xffu32 {
        let value = f32::from_bits(biased << 23);
        let ours = aterm_json::to_string(&value).unwrap();
        assert_eq!(ours.parse::<f32>(), Ok(value), "{ours} must read back");
        assert_eq!(
            digit_count(&ours),
            digit_count(&format!("{value:e}")),
            "2^{} (f32): {ours} is not the shortest form of {value:e}",
            i64::from(biased) - 127
        );
    }
}

/// Integers across every width and both signs are plain decimal.
#[test]
fn integer_formatting() {
    macro_rules! check {
        ($($v:expr),* $(,)?) => { $(
            assert_eq!(aterm_json::to_string(&$v).unwrap(), $v.to_string(), "{}", stringify!($v));
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

/// String escaping: only ASCII controls, in pinned forms, and every escaped
/// string reads back as itself.
#[test]
fn string_escaping_is_pinned_and_round_trips() {
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
    let mut golden = Golden::new();
    for case in cases {
        let text = aterm_json::to_string(&case).unwrap();
        golden.feed(text.as_bytes());
        let back: String = aterm_json::from_str(&text).expect("reads back");
        assert_eq!(back, case);
    }
    golden.check(STRINGS);
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
    /// for the other (`aterm-core`'s `CheckpointMeta` holds a `[u8; 8]`).
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
fn typed_models_serialize_to_pinned_bytes_and_read_back() {
    let mut golden = Golden::new();
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
        golden.feed(ours.as_bytes());
        let back: Doc = aterm_json::from_str(&ours).expect("reads back");
        assert_eq!(back, doc);
    }
    golden.check(TYPED);
}

#[test]
fn typed_deserialization_verdicts_and_values_are_pinned() {
    let mut golden = Golden::new();
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
        golden.typed(&aterm_json::from_str::<Doc>(input));
    }
    golden.check(TYPED_READS);
}

/// Every integer WIDTH against the same literals, at the top level — the typed
/// entry points `deserialize_u8` … `u128` each dispatch separately, and `-0`
/// and the 128-bit range are exactly where they used to diverge.
#[test]
fn scalar_integer_targets_are_pinned() {
    let mut golden = Golden::new();
    macro_rules! check {
        ($ty:ty, $input:expr) => {{
            golden.typed(&aterm_json::from_str::<$ty>($input));
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
    golden.check(SCALARS);
}

/// The 128-bit round trip, and `to_value` FAILS rather than hand back a
/// silently rounded float for a value `Value::Number` cannot hold.
#[test]
fn wide_integers_round_trip_or_fail() {
    for value in [u128::MAX, u128::from(u64::MAX) + 1, 0, 1] {
        let text = aterm_json::to_string(&value).expect("to_string");
        assert_eq!(text, value.to_string());
        assert_eq!(
            aterm_json::from_str::<u128>(&text).expect("round trip"),
            value
        );
    }
    for value in [i128::MIN, i128::MAX, i128::from(i64::MIN) - 1, -1, 0] {
        let text = aterm_json::to_string(&value).expect("to_string");
        assert_eq!(text, value.to_string());
        assert_eq!(
            aterm_json::from_str::<i128>(&text).expect("round trip"),
            value
        );
    }
    assert!(aterm_json::to_value(u128::MAX).is_err());
    assert!(aterm_json::to_value(i128::MIN).is_err());
    assert!(aterm_json::to_value(u64::MAX).is_ok());
    assert!(aterm_json::to_value(i64::MIN).is_ok());
}

/// A value that CANNOT serialize is refused, not turned into `null` — and
/// `json!` panics rather than substituting one.
#[test]
fn a_value_that_cannot_serialize_is_refused() {
    use std::collections::BTreeMap;
    let bad: BTreeMap<(u8, u8), u8> = [((1, 2), 3)].into_iter().collect();
    assert!(aterm_json::to_value(&bad).is_err());
    assert!(aterm_json::to_string(&bad).is_err());

    let ours = std::panic::catch_unwind(|| {
        let bad: BTreeMap<(u8, u8), u8> = [((1, 2), 3)].into_iter().collect();
        aterm_json::json!({ "field": bad })
    });
    assert!(ours.is_err(), "json! must not substitute null");
}

#[test]
fn deny_unknown_fields_denies() {
    for (input, ok) in [
        (r#"{"a":1,"b":"x"}"#, true),
        (r#"{"a":1,"b":"x","c":2}"#, false),
        (r#"{"b":"x"}"#, false),
        (r#"{"a":1,"b":"x","a":2}"#, false),
    ] {
        assert_eq!(aterm_json::from_str::<Strict>(input).is_ok(), ok, "{input}");
    }
}

/// The `json!` macro: nesting, arrays of objects, expression values, and a
/// `Value` spliced in.
#[test]
fn the_json_macro() {
    let model = "qwen3.5";
    let system = "be brief";
    let context = String::from("ctx");
    let keep_alive = aterm_json::json!(-1);
    let doc = aterm_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": context}
        ],
        "stream": false,
        "think": false,
        "keep_alive": keep_alive,
        "format": {
            "type": "object",
            "properties": {"description": {"type": "string"}},
            "required": ["description"],
            "additionalProperties": false
        },
        "options": {"temperature": 0, "num_predict": 64, "num_ctx": 4096}
    });
    assert_eq!(
        aterm_json::to_string(&doc).unwrap(),
        concat!(
            r#"{"format":{"additionalProperties":false,"properties":{"description":{"type":"string"}},"#,
            r#""required":["description"],"type":"object"},"keep_alive":-1,"#,
            r#""messages":[{"content":"be brief","role":"system"},{"content":"ctx","role":"user"}],"#,
            r#""model":"qwen3.5","options":{"num_ctx":4096,"num_predict":64,"temperature":0},"#,
            r#""stream":false,"think":false}"#,
        )
    );

    let index = 3usize;
    assert_eq!(
        aterm_json::to_string(&aterm_json::json!({
            "empty_obj": {},
            "empty_arr": [],
            "null": null,
            "computed": format!("frame-{index}.png"),
        }))
        .unwrap(),
        r#"{"computed":"frame-3.png","empty_arr":[],"empty_obj":{},"null":null}"#
    );
    assert_eq!(
        aterm_json::to_string(&aterm_json::json!("10m")).unwrap(),
        r#""10m""#
    );
    assert_eq!(
        aterm_json::to_string(&aterm_json::json!(null)).unwrap(),
        "null"
    );
    assert_eq!(
        aterm_json::to_string(&aterm_json::json!([1, "two", true, null, {"a": 1}])).unwrap(),
        r#"[1,"two",true,null,{"a":1}]"#
    );
}

/// `to_value` / `from_value` round-trip a typed model through the untyped tree.
#[test]
fn to_value_and_from_value_round_trip() {
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
    let value = aterm_json::to_value(&doc).expect("to_value");
    assert_eq!(
        aterm_json::to_string(&value).unwrap(),
        concat!(
            r#"{"name":"x","nested":null,"rows":[{"bytes":7,"detail":"d","extra":{"k":-1},"#,
            r#""flag":true,"nested_fixed":[10,11,12],"pair":[9,"p"],"phase":"done","ratio":0.5,"#,
            r#""stack":[1,2,3,4,5,6,7,8],"tags":["a"]}],"v":2}"#,
        )
    );
    let back: Doc = aterm_json::from_value(value).expect("from_value");
    assert_eq!(back, doc);
}

/// `Display` on a `Value` is a compact document — what every
/// `format!("{data}")` interpolation in this tree relies on.
#[test]
fn value_display_is_compact() {
    let value = aterm_json::json!({"b": 1, "a": [true, null, "x"]});
    assert_eq!(value.to_string(), r#"{"a":[true,null,"x"],"b":1}"#);
}

/// OUR parse is exactly the nearest double to the decimal written (Rust's
/// correctly rounded `dec2flt`); `serde_json`'s fast path is off by an ULP or
/// more on inputs as ordinary as `935092.5986328125`.
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
    }
}

/// JSON Pointer (RFC 6901): the LLM transport reaches into a provider's reply
/// with `/message/content` and `/choices/0/message/content`, so the resolution
/// rules — including the ones that must FAIL — matter.
#[test]
fn json_pointer() {
    let text = r#"{
        "message": {"content": "hi"},
        "choices": [{"message": {"content": "first"}}, {"message": {"content": "second"}}],
        "a/b": 1,
        "a~b": 2,
        "": 3,
        "list": [10, 20, 30]
    }"#;
    let value: aterm_json::Value = aterm_json::from_str(text).unwrap();
    for (pointer, expected) in [
        ("", Some(value.to_string())),
        ("/message/content", Some(r#""hi""#.to_string())),
        ("/choices/0/message/content", Some(r#""first""#.to_string())),
        (
            "/choices/1/message/content",
            Some(r#""second""#.to_string()),
        ),
        ("/choices/2/message/content", None),
        ("/list/0", Some("10".to_string())),
        ("/list/2", Some("30".to_string())),
        ("/list/3", None),
        ("/list/00", None),
        ("/list/+0", None),
        ("/list/-", None),
        ("/list/x", None),
        ("/a~1b", Some("1".to_string())),
        ("/a~0b", Some("2".to_string())),
        ("/", Some("3".to_string())),
        ("/missing", None),
        ("/message/content/deeper", None),
        ("message/content", None),
        (
            "/choices/0/message",
            Some(r#"{"content":"first"}"#.to_string()),
        ),
    ] {
        assert_eq!(
            value.pointer(pointer).map(ToString::to_string),
            expected,
            "pointer {pointer:?}"
        );
    }
}

/// `value["key"]` is total: a miss is `Null`, not a panic.
#[test]
fn index_sugar_is_total() {
    let text = r#"{"rows": 24, "cols": null, "list": [1, 2]}"#;
    let value: aterm_json::Value = aterm_json::from_str(text).unwrap();
    assert_eq!(value["rows"].as_u64(), Some(24));
    for key in ["cols", "missing", ""] {
        assert!(value[key].is_null(), "index {key:?}");
    }
    assert_eq!(value["list"][0].as_u64(), Some(1));
    assert_eq!(value["list"][1].as_u64(), Some(2));
    assert!(value["list"][2].is_null());
    assert!(value["list"][3].is_null());
    assert!(value["rows"]["nope"].is_null());
}

const GENERATED: &str = "74f9f81f8a37dea6";
const MUTATED: &str = "4daea7102798962d";
const ARBITRARY: &str = "090c30db3386449b";
const NAMED: &str = "6c8c9b749d92acdb";
const RECURSION: &str = "d1631af9cde22265";
const FLOATS: &str = "8a6248cbc700b80d";
const F32S: &str = "682492ba075ac644";
const STRINGS: &str = "63aa52cd03f3ebce";
const TYPED: &str = "51ca084c3767b92c";
const TYPED_READS: &str = "644c4f2c1fb5efbf";
const SCALARS: &str = "9afdc30f6689d326";
