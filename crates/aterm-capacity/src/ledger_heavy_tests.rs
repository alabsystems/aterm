// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A claim of the judgment turn ([`crate::Heavy`]) on the wire.

use super::*;

fn record(need: Need) -> Record {
    Record {
        timing: crate::TimingEvidence::default(),
        accounting: crate::Accounting::Holder,
        pid: 42,
        start_ms: 7,
        laid_at: None,
        seq: 0,
        checkout: "/ck".to_owned(),
        need,
        held: None,
        credit: Vec::new(),
        wrote: None,
        next_ms: None,
    }
}

/// A heavy claim's turn survives the claim file; one that names none reads as none; a turn of any
/// other shape makes the record unreadable, never a claim that asks nothing of the turn.
#[test]
fn a_heavy_claim_round_trips_and_a_malformed_one_is_no_record() {
    let heavy = record(Need::new("verify judgment turn").heavy(1_000, 300_000));
    let line = heavy.head_line();
    assert_eq!(
        Record::decode(&line, false).and_then(|r| r.need.heavy),
        Some(crate::Heavy {
            since_ms: 1_000,
            predicted_ms: 300_000,
        })
    );
    let plain = record(Need::new("cargo build").cores(2)).head_line();
    assert!(!plain.contains("heavy"), "{plain}");
    assert_eq!(
        Record::decode(&plain, false).map(|r| r.need.heavy),
        Some(None)
    );
    for bad in [
        aterm_json::json!({"since_ms": 1000}),
        aterm_json::json!({"since_ms": "x", "predicted_ms": 300_000}),
        aterm_json::json!(true),
    ] {
        let mut head: aterm_json::Value = aterm_json::from_str(line.trim_end()).expect("json");
        head.as_object_mut()
            .expect("an object")
            .insert("heavy".into(), bad.clone());
        assert!(
            Record::decode(&format!("{head}\n"), false).is_none(),
            "{bad}"
        );
    }
}
