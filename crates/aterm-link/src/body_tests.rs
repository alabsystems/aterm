// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_broadcast_kind_round_trips_and_is_closed() {
    let mut b = Body::new(7);
    b.kind = Some("task".to_string());
    b.text = "go".to_string();
    let wire = b.encode();
    let line = String::from_utf8_lossy(&wire).into_owned();
    assert!(line.contains(" kind=task"), "{line}");
    let (back, _) = Body::decode(&wire);
    assert_eq!(back.kind.as_deref(), Some("task"));

    let (invented, _) = Body::decode(b"v=1 t=1 kind=shout text=hi");
    assert_eq!(invented.kind, None, "an unknown kind is not a kind");
    assert_eq!(
        invented.unknown.get("kind").map(String::as_str),
        Some("shout"),
        "it is kept as an unknown field, losslessly"
    );
    let (plain, _) = Body::decode(b"v=1 t=1 text=hi");
    assert_eq!(plain.kind, None);
}

#[test]
fn a_text_body_round_trips_and_names_no_forbidden_field() {
    let mut b = Body::new(1_756_389_000_123);
    b.from = Some("s-abc".into());
    b.re = Some(90_312);
    b.dl = Some(240_000);
    b.text = "which branch has the fixture?".into();
    let wire = b.encode();
    let line = String::from_utf8(wire.clone()).expect("ascii line");
    assert!(line.starts_with("v=1 t=1756389000123 from=s-abc re=90312 dl=240000 text="));
    for forbidden in [" to=", " kind=", " id=", " trust=", " demoted="] {
        assert!(!line.contains(forbidden), "{line} carries {forbidden}");
    }
    let (got, raw) = Body::decode(&wire);
    assert_eq!(raw, None);
    assert_eq!(got, b);
}

#[test]
fn a_verdict_round_trips_and_an_unknown_one_is_not_a_verdict() {
    let mut b = Body::new(9);
    b.re = Some(41);
    b.verdict = Some("refused".into());
    b.text = "ack re=41 verdict=refused".into();
    let wire = b.encode();
    let line = String::from_utf8(wire.clone()).expect("ascii line");
    assert!(line.contains(" re=41 verdict=refused text="), "{line}");
    let (got, _) = Body::decode(&wire);
    assert_eq!(got.verdict.as_deref(), Some("refused"));
    assert_eq!(got, b);
    for word in ["handled", "deferred"] {
        assert!(is_verdict(word));
    }
    let (hostile, _) = Body::decode(b"v=1 t=1 re=41 verdict=handled%20now text=x");
    assert_eq!(hostile.verdict, None, "not one of the three words");
    assert_eq!(
        hostile.unknown.get("verdict").map(String::as_str),
        Some("handled%20now"),
        "kept as an unknown token, never as a verdict"
    );
}

#[test]
fn decoding_is_total_over_hostile_bytes() {
    for bytes in [
        &b""[..],
        b"\n",
        b"v=1 len=999999\nshort",
        b"v=1 t=notanumber re=also-not text=%",
        b"=====",
        &[0xff, 0xfe, b'\n', 0xff],
    ] {
        let (body, raw) = Body::decode(bytes);
        if let Some(raw) = raw {
            assert!(raw.len() <= bytes.len());
        }
        let _ = body.v;
    }
}

#[test]
fn an_unknown_token_survives() {
    let (body, _) = Body::decode(b"v=1 t=3 lang=en text=hi");
    assert_eq!(body.unknown.get("lang").map(String::as_str), Some("en"));
    let line = String::from_utf8(body.encode()).expect("ascii");
    assert!(line.contains(" lang=en"), "{line}");
}
