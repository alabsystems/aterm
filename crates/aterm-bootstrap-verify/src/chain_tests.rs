// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_deadline_is_one_utc_spelling_of_a_real_second() {
    let at = |text: &str| deadline(Some(&Value::String(text.into())));
    assert_eq!(at("1970-01-01T00:00:00Z"), Ok(0));
    assert_eq!(at("2030-01-01T00:00:00Z"), Ok(1_893_456_000));
    assert_eq!(at("2024-02-29T23:59:59Z"), Ok(1_709_251_199));
    for refused in [
        "2023-02-29T00:00:00Z",
        "2030-13-01T00:00:00Z",
        "2030-01-01T24:00:00Z",
        "2030-01-01T00:00:60Z",
        "0000-01-01T00:00:00Z",
        "2030-01-01T00:00:00+00:00",
        "2030-01-01",
        "9999-1-1T0:0:0Z",
    ] {
        assert_eq!(
            at(refused).expect_err(refused).0,
            "invalid roster expiry",
            "{refused}"
        );
    }
    assert!(deadline(Some(&Value::Integer(5))).is_err());
    assert!(deadline(None).is_err());
}

#[test]
fn a_tag_is_canonical_and_a_slug_two_words() {
    assert!(canonical_tag("v0.100.0") && canonical_tag("v18446744073709551616.0.0"));
    for refused in [
        "0.100.0",
        "v00.100.0",
        "v0.100",
        "v0.100.0.0",
        "v0.1a.0",
        "v0.100.00",
    ] {
        assert!(!canonical_tag(refused), "{refused}");
    }
    assert!(repository("example/aterm") && repository("a.b_c-d/e"));
    assert!(
        !repository("example/aterm/x")
            && !repository("/aterm")
            && !repository("a b/c")
            && !repository("./aterm")
            && !repository("../aterm")
            && !repository("example/.")
            && !repository("example/..")
    );
}

/// `read_bounded` refuses non-regular files (such as character devices or FIFOs) without
/// blocking on `open`.
/// NEGATIVE CONTROL: omitting `meta.is_file()` in `open_file` reads `/dev/null` as an empty
/// regular proof file instead of refusing it with `EINVAL`.
#[cfg(unix)]
#[test]
fn read_bounded_refuses_non_regular_files() {
    let dev_null = PyPath::new(std::ffi::OsStr::new("/dev/null"));
    let err = read_bounded(&dev_null, 64)
        .expect_err("device file refused")
        .0;
    assert!(
        err.contains("[Errno 22]") && err.contains("/dev/null"),
        "{err}"
    );
}

/// A parse error is one line, saying where, as tomllib's is: install.sh reads one.
#[test]
fn a_toml_error_is_one_line_saying_where() {
    let path = PyPath::new(std::ffi::OsStr::new("/r/aterm-appcast.toml"));
    let why = parse_toml(b"a = 1\nb = 2\na = 3\n", &path)
        .expect_err("a duplicate key")
        .0;
    assert!(
        why.starts_with("/r/aterm-appcast.toml is not TOML: ")
            && why.ends_with(" (at line 3, column 1)")
            && !why.contains('\n'),
        "{why}"
    );
    assert!(parse_toml(b"a = 1\n", &path).is_ok());
}

/// What TOML 1.0 admits and tomllib does not is no document: a byte order mark, a date
/// in year 0 and a leap second, anywhere in the tree; the nearest values it reads are.
#[test]
fn a_document_tomllib_refuses_is_refused() {
    let path = PyPath::new(std::ffi::OsStr::new("/r/aterm-appcast.toml"));
    let why = |text: &str| {
        parse_toml(text.as_bytes(), &path)
            .map(|_| ())
            .map_err(|e| e.0)
    };
    assert_eq!(
        why("\u{feff}a = 1\n"),
        Err("/r/aterm-appcast.toml is not TOML: Invalid statement (at line 1, column 1)".into())
    );
    for (text, value) in [
        ("a = 2026-06-30T23:59:60Z\n", "2026-06-30T23:59:60Z"),
        ("a = 0000-01-01\n", "0000-01-01"),
        ("a = 23:59:60\n", "23:59:60"),
        (
            "[t]\nb = [1, [0000-12-31T00:00:00]]\n",
            "0000-12-31T00:00:00",
        ),
    ] {
        assert_eq!(
            why(text),
            Err(format!(
                "/r/aterm-appcast.toml is not TOML: Invalid date or datetime {value}"
            )),
            "{text}"
        );
    }
    for text in [
        "a = \"\u{feff}\"\n",
        "a = 2026-06-30T23:59:59Z\n",
        "a = 0001-01-01\n",
        "a = 9999-12-31T23:59:59.999999+23:59\n",
    ] {
        assert_eq!(why(text), Ok(()), "{text}");
    }
}

/// A key is decoded as Python 3.11+'s `b64decode(validate=True)` decodes it: a spelling
/// it refuses is no key, and one it reads is then held to its canonical spelling.
#[test]
fn a_key_is_decoded_as_pythons_strict_base64() {
    let key = "oJql9HpnWYAv+VX43C0qFKXJnSO+l/hkEn/5ODRVpPA="; // gitleaks:allow
    assert_eq!(b64decode_strict(key).map(|raw| raw.len()), Some(32));
    assert!(canonical_key(key).is_some());
    for refused in [
        "AAA",
        "oJql9HpnWYAv+VX43C0qFKXJnSO+l/hkEn/5ODRVpPA",
        "AAAA=",
        "AAA==",
        "A===",
        "====",
        "AB=C",
        "AA==AA==",
        "AAAA AAAA",
        "AAAA\n",
        "AA-_",
    ] {
        assert_eq!(b64decode_strict(refused), None, "{refused}");
        assert!(canonical_key(refused).is_none(), "{refused}");
    }
    assert_eq!(b64decode_strict("AB=="), Some(vec![0]));
    assert_eq!(b64decode_strict(""), Some(Vec::new()));
    let alias = "oJql9HpnWYAv+VX43C0qFKXJnSO+l/hkEn/5ODRVpPB=";
    assert_eq!(b64decode_strict(alias), b64decode_strict(key));
    assert!(canonical_key(alias).is_none());
}
