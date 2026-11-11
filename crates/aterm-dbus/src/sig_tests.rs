// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn bad(sig: &str) -> &'static str {
    match parse_many(sig.as_bytes()) {
        Err(SigFault::Bad(what)) => what,
        other => panic!("{sig:?} parsed as {other:?}"),
    }
}

#[test]
fn every_atspi_signature_round_trips() {
    for sig in [
        "",
        "s",
        "so",
        "(so)",
        "a(so)",
        "a{sv}",
        "a{ss}",
        "siiva{sv}",
        "(so)(so)(so)iiassusau",
        "a((so)(so)(so)iiassusau)",
        "a(ua(so))",
        "a(sss)",
        "sa{sv}as",
        "(iiii)",
        "a{yd}",
        "aay",
        "(y(t))",
    ] {
        let types = parse_signature(sig).unwrap();
        assert_eq!(signature_of(&types), sig);
    }
}

#[test]
fn the_specification_limits_hold() {
    assert_eq!(bad("()"), "empty struct");
    assert_eq!(bad("a"), "signature ends inside a type");
    assert_eq!(bad("(s"), "signature ends inside a type");
    assert_eq!(bad("{sv}"), "dict entry outside an array");
    assert_eq!(bad("a{vs}"), "dict key is not a basic type");
    assert_eq!(bad("a{(s)s}"), "dict key is not a basic type");
    assert_eq!(bad("a{s}"), "dict entry holds other than two types");
    assert_eq!(bad("a{sss}"), "dict entry holds other than two types");
    assert_eq!(bad("s)"), "unbalanced close");
    assert_eq!(bad("m"), "unknown type code");
    assert_eq!(parse_many(b"ah"), Err(SigFault::Fd));
    for sig in ["h", "ah", "a{hs}", "(hh)", "sa{sv}h"] {
        assert_eq!(check_data(sig.as_bytes()), Ok(()), "{sig}");
    }
    assert_eq!(
        check_data(b"a{vh}"),
        Err(SigFault::Bad("dict key is not a basic type"))
    );
    assert_eq!(
        check_data(b"(h"),
        Err(SigFault::Bad("signature ends inside a type"))
    );
    assert_eq!(
        check_data(&[b'h'; 256]),
        Err(SigFault::Bad("signature longer than 255 bytes"))
    );
    let deep_arrays = "a".repeat(32) + "y";
    assert!(parse_many(deep_arrays.as_bytes()).is_ok());
    assert_eq!(bad(&("a".repeat(33) + "y")), "arrays nested deeper than 32");
    let deep = |n: usize| "(".repeat(n) + "y" + &")".repeat(n);
    assert!(parse_many(deep(32).as_bytes()).is_ok());
    assert_eq!(bad(&deep(33)), "structs nested deeper than 32");
    assert_eq!(bad(&"y".repeat(256)), "signature longer than 255 bytes");
    assert!(parse_many("y".repeat(255).as_bytes()).is_ok());
}

#[test]
fn a_variant_signature_is_one_complete_type() {
    assert_eq!(Type::parse("(so)").unwrap().signature(), "(so)");
    assert!(matches!(Type::parse(""), Err(Error::Invalid(_))));
    assert!(matches!(Type::parse("ss"), Err(Error::Invalid(_))));
    assert!(matches!(Type::parse("h"), Err(Error::Unsupported(_))));
}
