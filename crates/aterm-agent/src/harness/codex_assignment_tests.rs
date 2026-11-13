// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn zero_assignment_is_an_explicit_startup_mode_with_no_boundary() {
    // Signed-book data validation only; no observed peer, native permit or
    // admitted grant is fabricated by deserializing these fixture values.
    let caller = Caller {
        sid: "s-startup-fixture".into(),
        pid: 42,
        birth: 123,
    };
    let mut book: Book = aterm_json::from_value(aterm_json::json!({
        "schema":2,"caller":{"sid":caller.sid,"pid":42,"birth":123},
        "consumer":{"pid":42,"birth":123,"parent":2,"group":42,"session":42,
            "image":[47,116,101,115,116],"device":1,"inode":2},
        "clock":0,
        "current":{"binding":{
            "request":{"id":"a".repeat(32),"caller":"b".repeat(64),"label":"fixture","revision":"c".repeat(64)},
            "reference":{"operation":"a".repeat(32),"capability":"d".repeat(64),
                "issuance_digest":"e".repeat(64),"producer_mac":"f".repeat(64)},"generation":1},
            "generation":0,"previous_generation":0,
            "authority":{"startup":{"nonce":"1".repeat(32)}},
            "boundary":null,"phase":"startup"}
    })).unwrap();
    assert!(book.valid(&caller));
    book.current.boundary = Some("2".repeat(32));
    assert!(!book.valid(&caller));
    book.current.boundary = None;
    book.current.phase = Phase::Applied;
    assert!(!book.valid(&caller));
    book.current.authority = SourceAuthority::Assignment {
        operation: "3".repeat(32),
    };
    assert!(!book.valid(&caller));
    book.current.generation = 1;
    book.clock = 1;
    book.current.boundary = Some("2".repeat(32));
    assert!(book.valid(&caller));
    book.current.previous_generation = 1;
    assert!(!book.valid(&caller));
    book.current.previous_generation = 0;
    book.current.phase = Phase::Startup;
    assert!(!book.valid(&caller));
    book.current.phase = Phase::Applied;
    book.schema = 1;
    assert!(!book.valid(&caller));
}

#[test]
fn source_authority_refuses_mixed_or_extra_fields() {
    for malformed in [
        aterm_json::json!({"startup":{"nonce":"1".repeat(32),"operation":"2".repeat(32)}}),
        aterm_json::json!({"startup":{"nonce":"1".repeat(32)},"assignment":{"operation":"2".repeat(32)}}),
        aterm_json::json!({"assignment":{"operation":"2".repeat(32),"path":"/auth.json"}}),
    ] {
        assert!(aterm_json::from_value::<SourceAuthority>(malformed).is_err());
    }
}
