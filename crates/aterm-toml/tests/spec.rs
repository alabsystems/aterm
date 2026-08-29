// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TOML 1.0.0 conformance, checked BOTH ways against the oracle: what the spec
//! says must parse, and — the half that matters for a config parser — what the
//! spec says must be REJECTED.
//!
//! A parser that accepts a duplicate key, or lets a second `[table]` header
//! silently reopen the first, makes the file an operator reads and the file the
//! program obeys two different documents. Every rule below is one of those.
//!
//! # The canonical suite
//!
//! The corpus here is hand-written, so it can only pin rules someone thought
//! of — and a rule nobody thought of is exactly how the "a dotted key closes
//! its table to sub-tables too" bug survived: the [`VALID`] entry copied from
//! the specification's worked example stopped one line short of the line that
//! disproved it. So the authority is `toml-test`, and this file is the subset
//! of it that this tree's own history says is worth keeping close.
//!
//! MEASURED against `toml-test-data` 2.13.0 (777 documents) on 2026-08-27:
//!
//! ```text
//! valid:    210 pass / 6 fail        invalid: 492 pass / 0 fail
//! ```
//!
//! All six are TOML 1.1 DRAFTS, and `toml` 0.8.23 and `toml_edit` 0.22.27 both
//! reject the same six — parity with the crates being replaced, not a gap:
//! `datetime/no-seconds`, `inline-table/newline`, `inline-table/newline-comment`
//! and `key/empty-05` (newlines inside inline tables), `string/escape-esc`
//! (`\e`) and `string/hex-escape` (`\x`). Those are the omissions `lib.rs`
//! names. `valid/spec-1.0.0/table-9`, `valid/utf8-bom-01` and
//! `valid/utf8-bom-02` pass AND round-trip byte for byte, which the oracle does
//! not manage for the BOM pair; `invalid/control/comment-del` is refused.
//!
//! The suite is NOT a dev-dependency: `toml-test-data` carries 4.4 MB of assets
//! and drags `include_dir` behind it, and dropping 509 deliberately-invalid
//! `.toml` files into the tree would break the repository-corpus tests in
//! `differential.rs` and `roundtrip.rs`, which walk every `.toml` in the repo.
//! The cases it caught are transcribed below instead.

mod common;

use common::values_agree;

/// Both implementations must accept these, and agree on what they mean.
const VALID: &[&str] = &[
    // Keys: bare, quoted, dotted, and the awkward spellings of each.
    "key = 1\nbare_key = 2\nbare-key = 3\n1234 = 4\n",
    "\"127.0.0.1\" = 1\n\"character encoding\" = 2\n\"ʎǝʞ\" = 3\n'key2' = 4\n'quoted \"value\"' = 5\n",
    "physical.color = \"orange\"\nphysical.shape = \"round\"\nsite.\"google.com\" = true\n",
    "fruit.apple.smooth = true\nfruit.orange = 2\n",
    "3.14159 = \"pi\"\n",
    "\"\" = 'blank'\n",
    "'' = 'blank'\n",
    // Strings: all four forms, escapes, the line-ending backslash.
    "str = \"I'm a string. \\\"You can quote me\\\". Name\\tJos\\u00E9\\nLocation\\tSF.\"\n",
    "str1 = \"\"\"\nRoses are red\nViolets are blue\"\"\"\n",
    "str2 = \"\"\"\\\n       The quick brown \\\n       fox jumps over \\\n       the lazy dog.\\\n       \"\"\"\n",
    "winpath = 'C:\\\\Users\\\\nodejs\\\\templates'\nquoted = 'Tom \"Dubs\" Preston-Werner'\nregex = '<\\\\i\\\\c*\\\\s*>'\n",
    "regex2 = '''I [dw]on't need \\\\d{2} apples'''\n",
    "lines = '''\nThe first newline is\ntrimmed in raw strings.\n'''\n",
    "quot = \"\"\"Here are two quotation marks: \"\". Simple enough.\"\"\"\n",
    "quot15 = '''Here are fifteen quotation marks: \"\"\"\"\"\"\"\"\"\"\"\"\"\"\"'''\n",
    "wide = \"\\U0001F600 \\u00E9 \\u0000\"\n",
    "ctrl = \"\\b\\t\\n\\f\\r\\\"\\\\\"\n",
    "apos = \"\"\"Here are three apostrophes: '''.\"\"\"\n",
    "str = \"\"\"ends with two quotes\"\"\"\"\"\n",
    // Integers.
    "int1 = +99\nint2 = 42\nint3 = 0\nint4 = -17\n",
    "int5 = 1_000\nint6 = 5_349_221\nint7 = 53_49_221\nint8 = 1_2_3_4_5\n",
    "hex1 = 0xDEADBEEF\nhex2 = 0xdeadbeef\nhex3 = 0xdead_beef\noct1 = 0o01234567\noct2 = 0o755\nbin1 = 0b11010110\n",
    "max = 9223372036854775807\nmin = -9223372036854775808\n",
    // Floats.
    "f1 = +1.0\nf2 = 3.1415\nf3 = -0.01\nf4 = 5e+22\nf5 = 1e06\nf6 = -2E-2\nf7 = 6.626e-34\nf8 = 224_617.445_991_228\n",
    "sf1 = inf\nsf2 = +inf\nsf3 = -inf\nsf4 = nan\nsf5 = +nan\nsf6 = -nan\n",
    "zeroes = [0.0, +0.0, -0.0, 0e0, +0e0, -0e0]\n",
    // Booleans.
    "bool1 = true\nbool2 = false\n",
    // Date-times, all four shapes and both separators.
    "odt1 = 1979-05-27T07:32:00Z\nodt2 = 1979-05-27T00:32:00-07:00\nodt3 = 1979-05-27T00:32:00.999999-07:00\n",
    "odt4 = 1979-05-27 07:32:00Z\n",
    "ldt1 = 1979-05-27T07:32:00\nldt2 = 1979-05-27T00:32:00.999999\n",
    "ld1 = 1979-05-27\n",
    "lt1 = 07:32:00\nlt2 = 00:32:00.999999\n",
    "leap = 1988-12-31T23:59:60Z\nfeb = 2024-02-29\n",
    // Arrays.
    "integers = [1, 2, 3]\ncolors = ['red', 'yellow', 'green']\nnested = [[1, 2], [3, 4, 5]]\n",
    "mixed = [0.1, 0.2, 0.5, 1, 2, 5]\nstrings = [\"all\", 'strings', \"\"\"are the same\"\"\", '''type''']\n",
    "integers2 = [\n  1, 2, 3\n]\nintegers3 = [\n  1,\n  2, # this is ok\n]\n",
    "empty = []\nnested_empty = [[], [[]]]\n",
    // Inline tables.
    "name = { first = \"Tom\", last = \"Preston-Werner\" }\npoint = { x = 1, y = 2 }\nanimal = { type.name = \"pug\" }\n",
    "empty = {}\n",
    // Tables and arrays of tables.
    "[table]\n[table-1]\nkey1 = \"some string\"\nkey2 = 123\n\n[table-2]\nkey1 = \"another string\"\n",
    "[dog.\"tater.man\"]\ntype.name = \"pug\"\n",
    "[a.b.c]\n[ d.e.f ]\n[ g .  h  . i ]\n[ j . \"ʞ\" . 'l' ]\n",
    "[x.y.z.w]\n[x]\n",
    "[fruit]\napple.color = \"red\"\napple.taste.sweet = true\n",
    // The spec's own worked example, INCLUDING the line it annotates "you can
    // add sub-tables". A dotted key closes its table to REDEFINITION, not to
    // extension, and the eight shapes below are the whole matrix of that rule:
    // plain and array-of-tables headers, one and two dotted segments, at the
    // root and under a `[table]` and under a `[[array]]`.
    "[fruit]\napple.color = \"red\"\napple.taste.sweet = true\n\n[fruit.apple.texture]\nsmooth = true\n",
    "a.b = 1\n[a.c]\n",
    "a.b = 1\n[[a.c]]\n",
    "a.b.c = 1\n[a.b.d]\n",
    "a.b.c = 1\n[a.d]\n",
    "a.b.c = 1\n[[a.b.d]]\n",
    "[t]\nb.c = 1\n[t.b.d]\n",
    "[t]\nb.c = 1\n[[t.b.d]]\n",
    "[[t]]\nb.c = 1\n[t.b.d]\n",
    "[[products]]\nname = \"Hammer\"\nsku = 738594937\n\n[[products]]\n\n[[products]]\nname = \"Nail\"\n",
    "[[fruits]]\nname = \"apple\"\n\n[fruits.physical]\ncolor = \"red\"\n\n[[fruits.varieties]]\nname = \"red delicious\"\n\n[[fruits.varieties]]\nname = \"granny smith\"\n",
    "[[a.b]]\nx = 1\n\n[a.b.c]\ny = 2\n\n[[a.b]]\nx = 3\n",
    "[\"\"]\nk = 1\n",
    "[a]\nb = { c = [ { d = 1 }, { e = 2 } ] }\n",
    // Comments and whitespace corners.
    "# just a comment\n",
    "\n\n\n",
    "key = \"value\" # comment with \"quotes\" and 'apostrophes' and # hashes\n",
    "key = 1\r\nother = 2\r\n",
    // A `\r\n` INSIDE a multi-line string is normalized to `\n` in the VALUE,
    // exactly as both retired crates do, while the raw bytes still round-trip.
    // toml-test has no case for this; a config saved by a Windows editor does.
    "x = \"\"\"a\r\nb\"\"\"\n",
    "x = '''a\r\nb'''\n",
    // A leading UTF-8 BOM (`valid/utf8-bom-01`, `utf8-bom-02` in toml-test).
    // PowerShell 5 writes one by default and aterm ships a Windows cell.
    "\u{feff}x = 1\n",
    "\u{feff}[a]\nb = 1\n",
    "\u{feff}# just a comment\n",
];

/// Both implementations must REFUSE these. Each line names the rule.
const INVALID: &[(&str, &str)] = &[
    (
        "name = \"Tom\"\nname = \"Pradyun\"\n",
        "a key defined twice",
    ),
    (
        "spelling = \"favorite\"\n\"spelling\" = \"favourite\"\n",
        "the same key under two spellings",
    ),
    (
        "fruit.apple = 1\nfruit.apple.smooth = true\n",
        "a dotted key extending a scalar",
    ),
    (
        "[fruit]\napple.color = \"red\"\n\n[fruit.apple]\n",
        "a header reopening a dotted-key table",
    ),
    (
        "[fruit]\napple.taste.sweet = true\n\n[fruit.apple.taste]\n",
        "a header reopening a dotted sub-table",
    ),
    ("[a]\nb = 1\n\n[a]\nc = 2\n", "the same table header twice"),
    (
        "[a]\n[a.b]\n[a]\n",
        "a table header repeated after its children",
    ),
    ("a = 1\n[a]\n", "a header shadowing a value"),
    (
        "[[fruit]]\nname = \"apple\"\n\n[[fruit.variety]]\nname = \"red\"\n\n[fruit.variety]\n",
        "a table over an array of tables",
    ),
    ("[fruit]\n[[fruit]]\n", "an array of tables over a table"),
    ("fruit = []\n[[fruit]]\n", "appending to a static array"),
    (
        "product = { name = \"Hammer\" }\n[product]\nsku = 1\n",
        "a header reopening an inline table",
    ),
    (
        "product = { name = \"Hammer\" }\nproduct.sku = 1\n",
        "a dotted key extending an inline table",
    ),
    ("key = # INVALID\n", "a missing value"),
    ("key =\n", "a missing value at end of line"),
    ("= 1\n", "a missing key"),
    ("key\n", "a key with no value"),
    (
        "first = \"Tom\" last = \"Preston-Werner\"\n",
        "two pairs on one line",
    ),
    ("str = \"no closing quote\n", "an unterminated basic string"),
    (
        "str = 'no closing quote\n",
        "an unterminated literal string",
    ),
    (
        "str = \"\\x invalid escape\"\n",
        "`\\x` is a TOML 1.1 draft, not 1.0",
    ),
    ("str = \"\\e\"\n", "`\\e` is a TOML 1.1 draft, not 1.0"),
    (
        "str = \"\\uD800\"\n",
        "a lone surrogate is not a scalar value",
    ),
    ("int = 0_1\n", "an underscore not flanked by digits"),
    ("int = 1__2\n", "a doubled underscore"),
    ("int = 1_\n", "a trailing underscore"),
    ("int = 010\n", "a leading zero"),
    ("int = 0x\n", "an empty hex literal"),
    ("int = +0x1\n", "a signed radix literal"),
    ("int = 9223372036854775808\n", "an integer past i64"),
    ("flt = .7\n", "a float with no integer part"),
    ("flt = 7.\n", "a float with no fractional digits"),
    (
        "flt = 3.e+20\n",
        "a float with an empty fraction before the exponent",
    ),
    ("flt = 1e\n", "an empty exponent"),
    ("bool = True\n", "a capitalised boolean"),
    ("bool = TRUE\n", "an upper-case boolean"),
    ("arr = [1, 2\n", "an unterminated array"),
    (
        "t = { a = 1,\n b = 2 }\n",
        "a newline inside an inline table",
    ),
    (
        "t = { a = 1, }\n",
        "a trailing comma inside an inline table",
    ),
    ("[]\n", "an empty table header"),
    ("[a.]\n", "a trailing dot in a header"),
    ("[.a]\n", "a leading dot in a header"),
    ("[a\n", "an unterminated header"),
    ("[[a]\n", "a mismatched array-of-tables header"),
    ("dt = 1979-05-27T07:32\n", "a TOML 1.0 time without seconds"),
    ("dt = 1979-13-01\n", "an impossible month"),
    ("dt = 2023-02-29\n", "a day that does not exist"),
    ("dt = 1979-05-27T25:00:00Z\n", "an impossible hour"),
    ("key = value\n", "a bare unquoted string"),
    ("key = \"a\"extra\n", "trailing characters after a value"),
    (
        "x = \"\\u+041\"\n",
        "a signed unicode escape is not four HEXDIG",
    ),
    (
        "x = \"\\U+0000041\"\n",
        "a signed long unicode escape is not eight HEXDIG",
    ),
    (
        "x = \"\\uab\u{20ac}\"\n",
        "a multi-byte character inside a unicode escape window",
    ),
    (
        "x = \"\\u00\u{e9}41\"\n",
        "a multi-byte character straddling the end of the window",
    ),
    (
        "# comment\u{7f}\n",
        "DEL in a comment (`invalid/control/comment-del`; the published ABNF's \
         %x20-7F is a known erratum)",
    ),
];

#[test]
fn everything_the_spec_allows_parses_and_agrees_with_the_oracle() {
    for source in VALID {
        let ours: aterm_toml::Value = match aterm_toml::from_str(source) {
            Ok(v) => v,
            Err(e) => panic!("we rejected valid TOML {source:?}: {e}"),
        };
        let theirs: toml::Value = match toml::from_str(source) {
            Ok(v) => v,
            Err(e) => panic!("the oracle rejected {source:?}: {e}"),
        };
        values_agree(&ours, &theirs).unwrap_or_else(|why| panic!("{source:?}: {why}"));

        // And the document half reproduces the source exactly.
        let document: aterm_toml::edit::DocumentMut = source
            .parse()
            .unwrap_or_else(|e| panic!("the document half rejected {source:?}: {e}"));
        assert_eq!(
            document.to_string(),
            *source,
            "round-trip changed {source:?}"
        );
    }
}

#[test]
fn everything_the_spec_forbids_is_refused_by_both() {
    let mut ours_accepted = Vec::new();
    let mut oracle_accepted = Vec::new();
    for (source, rule) in INVALID {
        if aterm_toml::from_str::<aterm_toml::Value>(source).is_ok() {
            ours_accepted.push(format!("  {rule}: {source:?}"));
        }
        if toml::from_str::<toml::Value>(source).is_ok() {
            oracle_accepted.push(format!("  {rule}: {source:?}"));
        }
    }
    assert!(
        ours_accepted.is_empty(),
        "we accepted invalid TOML:\n{}",
        ours_accepted.join("\n")
    );
    assert!(
        oracle_accepted.is_empty(),
        "the oracle accepted these, so the rule as written is wrong:\n{}",
        oracle_accepted.join("\n")
    );
}

/// Rejections must locate themselves, or the config editor has nothing to
/// underline.
#[test]
fn parse_errors_carry_a_span_inside_the_source() {
    for (source, rule) in INVALID {
        let error =
            aterm_toml::from_str::<aterm_toml::Value>(source).expect_err("checked invalid above");
        let Some(span) = error.span() else {
            panic!("{rule}: {source:?} produced an error with no span: {error}");
        };
        assert!(
            span.start <= source.len() && span.end <= source.len() && span.start <= span.end,
            "{rule}: span {span:?} is outside {source:?}"
        );
        // And it must land on CHARACTER boundaries: `Error::span()` is public
        // API, `crates/aterm-gui` slices the source with ranges derived from
        // it, and a range that splits a character panics when sliced.
        assert!(
            source.is_char_boundary(span.start) && source.is_char_boundary(span.end),
            "{rule}: span {span:?} splits a character in {source:?}"
        );
    }
}

/// A super-table may be declared AFTER the header that implied it — the one
/// redefinition the spec explicitly allows.
#[test]
fn an_implicit_super_table_may_be_declared_later() {
    let source = "[a.b]\nx = 1\n\n[a]\ny = 2\n";
    let value: aterm_toml::Value = aterm_toml::from_str(source).expect("legal per the spec");
    assert_eq!(
        value
            .get("a")
            .and_then(|a| a.get("y"))
            .and_then(aterm_toml::Value::as_integer),
        Some(2)
    );
    assert_eq!(
        value
            .get("a")
            .and_then(|a| a.get("b"))
            .and_then(|b| b.get("x"))
            .and_then(aterm_toml::Value::as_integer),
        Some(1)
    );
    let document: aterm_toml::edit::DocumentMut = source.parse().expect("parses");
    assert_eq!(
        document.to_string(),
        source,
        "header order must survive the round-trip"
    );
}

/// The multi-line quote-run rule: up to two of the delimiter may sit against
/// the closing one.
#[test]
fn multiline_strings_handle_delimiter_runs() {
    for (source, expected) in [
        ("s = \"\"\"a\"\"\"\n", "a"),
        ("s = \"\"\"a\"\"\"\"\n", "a\""),
        ("s = \"\"\"a\"\"\"\"\"\n", "a\"\""),
        ("s = '''a''''\n", "a'"),
        ("s = '''a'''''\n", "a''"),
        ("s = \"\"\"\"a\"\"\"\n", "\"a"),
    ] {
        let value: aterm_toml::Value =
            aterm_toml::from_str(source).unwrap_or_else(|e| panic!("{source:?}: {e}"));
        assert_eq!(
            value.get("s").and_then(aterm_toml::Value::as_str),
            Some(expected),
            "{source:?}"
        );
        let theirs: toml::Value =
            toml::from_str(source).unwrap_or_else(|e| panic!("oracle: {source:?}: {e}"));
        assert_eq!(
            theirs.get("s").and_then(toml::Value::as_str),
            Some(expected)
        );
    }
    assert!(
        aterm_toml::from_str::<aterm_toml::Value>("s = \"\"\"a\"\"\"\"\"\"\n").is_err(),
        "six quotes in a row is not a legal close"
    );
}

/// The one deliberate divergence from the oracle, pinned so it cannot drift
/// into being accidental.
///
/// TOML does not say what an implementation should do with a finite literal
/// that overflows binary64. `toml` 0.8.23 refuses `1e400` and accepts `-1e400`
/// as negative infinity — an asymmetry that would let `timeout = -1e400` slip
/// into a config while the positive typo is caught. This crate refuses both.
#[test]
fn overflowing_float_literals_are_refused_in_both_signs() {
    for source in [
        "x = 1e400\n",
        "x = -1e400\n",
        "x = 1e309\n",
        "x = -1e309\n",
        "x = 92E3372036854775807\n",
    ] {
        assert!(
            aterm_toml::from_str::<aterm_toml::Value>(source).is_err(),
            "{source:?} overflows binary64 and must be refused"
        );
    }
    // Underflow is not overflow: it has an exact representable answer.
    let value: aterm_toml::Value = aterm_toml::from_str("x = 1e-400\n").expect("underflow is fine");
    assert_eq!(
        value.get("x").and_then(aterm_toml::Value::as_float),
        Some(0.0)
    );
    assert_eq!(
        toml::from_str::<toml::Value>("x = 1e-400\n")
            .expect("oracle agrees")
            .get("x")
            .and_then(toml::Value::as_float),
        Some(0.0)
    );

    // The evidence for the asymmetry, so a future reader does not have to
    // re-derive it: the oracle really does accept the negative one.
    assert!(toml::from_str::<toml::Value>("x = 1e400\n").is_err());
    assert!(toml::from_str::<toml::Value>("x = -1e400\n").is_ok());
}

// ---------------------------------------------------------------------------
// How an error PRINTS, and where it points
// ---------------------------------------------------------------------------

/// The rendered diagnostic is byte-identical to the oracle's.
///
/// This is not cosmetic. `native_config_language::analyze` puts
/// `error.to_string()` — whitespace-flattened — into the config editor's status
/// bar, so the SOURCE LINE inside the annotated snippet is the part that tells
/// the operator `font_px` is the key at fault. An error that named only a line
/// and column would pass every type check and make the editor useless, which is
/// exactly what happened the first time this crate was wired in.
#[test]
fn a_schema_error_renders_exactly_what_toml_renders() {
    #[derive(Debug, serde::Deserialize)]
    #[allow(dead_code)]
    struct Config {
        font_px: f64,
    }

    for source in [
        "font_px = \"huge\"\n",
        "font_px = \"not-a-number\"\n",
        "font_px = true\n",
        "# a comment first\nfont_px = [1]\n",
    ] {
        let ours = aterm_toml::from_str::<Config>(source).expect_err("refused");
        let theirs = toml::from_str::<Config>(source).expect_err("the oracle refuses too");
        assert_eq!(
            ours.to_string(),
            theirs.to_string(),
            "the rendered diagnostic differs for {source:?}"
        );
        assert_eq!(
            ours.span(),
            theirs.span(),
            "the span differs for {source:?}"
        );
    }
}

/// END OF INPUT IS ON THE LAST AUTHORED LINE, and its span is EMPTY.
///
/// Both halves are the oracle's convention and both are load-bearing:
/// `native_config_language::parser_diagnostic_range` turns an empty span at or
/// past the end of input into a caret on the last authored byte, so a caret the
/// operator can see. A span covering the whole unterminated construct is not
/// empty, misses that path, and underlines the opening bracket instead —
/// measured, and the reason this test exists.
#[test]
fn end_of_input_errors_point_where_the_oracle_points() {
    for source in [
        "x = [ ",
        "x = [1, 2\n",
        "# Manual\nfont_px = [ \n",
        "x = [\n\n",
    ] {
        let ours = source
            .parse::<aterm_toml::edit::DocumentMut>()
            .expect_err("refused");
        let theirs = source
            .parse::<toml_edit::DocumentMut>()
            .expect_err("the oracle refuses too");
        assert_eq!(ours.span(), theirs.span(), "span differs for {source:?}");
        let span = ours.span().expect("a refusal carries a span");
        assert!(span.is_empty(), "{source:?} must report an EMPTY span");
        assert_eq!(span.start, source.len(), "and it must be at end of input");
        // The rendered position, which is what the editor's "Ln 2, Col 13"
        // comes from: the LAST AUTHORED line, not the blank one after it.
        let ours_first = ours.to_string();
        let theirs_first = theirs.to_string();
        let position = |text: &str| {
            text.lines()
                .next()
                .expect("the first line carries the position")
                .to_owned()
        };
        assert_eq!(
            position(&ours_first),
            position(&theirs_first),
            "line/column differ for {source:?}"
        );
    }
}

/// A DELIBERATE DIVERGENCE, pinned so it cannot rot into an unnoticed one.
///
/// Our spans underline the offending TOKEN; the oracle's mark the single byte
/// where its parser combinator gave up. Measured over the [`INVALID`] corpus,
/// the two agree on 13 of 55 and differ on the rest — `int = 010` is ours
/// `6..9` (the literal) against the oracle's `7..8` (the byte after the zero).
/// Ours is the more useful underline and nothing in this tree depends on the
/// other convention: the ONE place that did — an empty end-of-input span — is
/// matched exactly, above.
///
/// What IS required of every span is asserted here for all 55: in range, and
/// pointing at a byte boundary of the source.
#[test]
fn token_spans_are_wider_than_the_oracles_and_that_is_deliberate() {
    let mut agree = 0usize;
    for (source, why) in INVALID {
        let ours = source
            .parse::<aterm_toml::edit::DocumentMut>()
            .expect_err(why);
        let theirs = source
            .parse::<toml_edit::DocumentMut>()
            .expect_err("the oracle refuses it too");
        let span = ours.span().expect("every refusal carries a span");
        assert!(
            span.end <= source.len() && span.start <= span.end,
            "{why}: span {span:?} is outside {source:?}"
        );
        assert!(
            source.is_char_boundary(span.start) && source.is_char_boundary(span.end),
            "{why}: span {span:?} splits a character in {source:?}"
        );
        if ours.span() == theirs.span() {
            agree += 1;
        }
    }
    assert_eq!(
        agree, 13,
        "the span conventions moved — re-read the note above before re-pinning"
    );
    // The named example, so the note above is checkable rather than folklore.
    let ours = "int = 010\n"
        .parse::<aterm_toml::edit::DocumentMut>()
        .expect_err("a leading zero is refused");
    let theirs = "int = 010\n"
        .parse::<toml_edit::DocumentMut>()
        .expect_err("the oracle refuses it too");
    assert_eq!(ours.span(), Some(6..9), "ours underlines the literal");
    assert_eq!(theirs.span(), Some(7..8), "the oracle marks one byte");
}
