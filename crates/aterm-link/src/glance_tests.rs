// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn row(subject: &str, body: &str) -> Row {
    Row::parse("f1", 7, subject, body.as_bytes()).expect("a presence row")
}

#[test]
fn a_row_carries_every_token_and_always_the_two_that_matter() {
    let r = row(
        "/f/f1/pub/n-a/s-0123456789abcdef0123/presence",
        "v=1 t=5 inc=2 state=live hold=0 fabric=connected weather=sunny",
    );
    assert_eq!(r.node, "n-a");
    assert_eq!(r.owner, "s-0123456789abcdef0123");
    assert_eq!(r.field("state"), "live");
    assert_eq!(r.field("fabric"), "connected");
    assert_eq!(r.field("weather"), "sunny", "an unknown token must survive");
    assert_eq!(r.field("attention"), ABSENT, "absent reads as a dash");
    let bare = row("/f/f1/pub/n-a/node/presence", "v=1 t=5 state=gone");
    assert_eq!(bare.owner, "node");
    assert_eq!(bare.field("fabric"), ABSENT);
    assert_eq!(bare.field("attention"), ABSENT);
}

#[test]
fn only_a_presence_subject_of_this_fleet_becomes_a_row() {
    for bad in [
        "/f/f2/pub/n-a/node/presence",     // another fleet
        "/f/f1/pub/n-a/node/ev",           // another leaf
        "/f/f1/in/n-a/node/presence",      // another face
        "/f/f1/pub/andrew/node/presence",  // a node with no class
        "/f/f1/pub/n-a/nodes/presence",    // an owner that is neither
        "/f/f1/pub/n-a/h-andrew/presence", // an owner that is not a session
        "/f/f1/pub/x/n-a/node/presence",   // eight segments
        "f/f1/pub/n-a/node/presence",      // no leading slash
    ] {
        assert!(Row::parse("f1", 1, bad, b"v=1").is_none(), "{bad}");
    }
}

#[test]
fn attention_is_capped_on_a_char_boundary() {
    let long = format!("attention={}", "é".repeat(400));
    let r = row("/f/f1/pub/n-a/node/presence", &long);
    let got = r.field("attention");
    assert!(got.len() <= ATTENTION_CAP, "{}", got.len());
    assert!(got.ends_with('…'));
    let short = row("/f/f1/pub/n-a/node/presence", "attention=needs%20you");
    assert_eq!(short.field("attention"), "needs%20you");
}

#[test]
fn no_publisher_chooses_how_big_a_row_is() {
    let huge = "z".repeat(100_000);
    let body =
        format!("state=live detail={huge} host={huge} {huge}=x attention={huge} sneaky={huge}");
    let r = row("/f/f1/pub/n-a/node/presence", &body);
    for (k, v) in &r.fields {
        assert!(k.len() <= FIELD_CAP, "key of {} bytes", k.len());
        assert!(v.len() <= FIELD_CAP, "{k} value of {} bytes", v.len());
    }
    assert_eq!(r.field("state"), "live", "a short value is untouched");
    assert!(r.field("detail").ends_with('…'), "a cut is marked");

    let many: String = (0..5000).map(|i| format!("k{i}=v{i} ")).collect();
    let r = row("/f/f1/pub/n-a/node/presence", &many);
    assert!(
        r.fields.len() <= FIELDS_MAX + GUARANTEED.len(),
        "{}",
        r.fields.len()
    );
    let padded = format!("{many} attention=needs%20you");
    let r = row("/f/f1/pub/n-a/node/presence", &padded);
    assert_eq!(r.field("attention"), "needs%20you");
    assert_eq!(r.field("fabric"), ABSENT);
}

#[test]
fn no_body_token_can_shadow_a_cap_forced_row_fact() {
    let r = row(
        "/f/f1/pub/n-evil/node/presence",
        "v=1 state=live owner=s-victim0000 node=n-lab subject=/f/f1/nope offset=99",
    );
    assert_eq!(r.node, "n-evil", "the subject names the node, not the body");
    assert_eq!(r.owner, "node");
    for k in RESERVED {
        assert!(!r.fields.contains_key(k), "{k}= survived into the fields");
    }
    assert_eq!(r.field("state"), "live");
}
