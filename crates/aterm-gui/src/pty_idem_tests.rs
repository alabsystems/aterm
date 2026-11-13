// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

/// Only a keyed attempt with nothing to type is refused; an unkeyed empty
/// write and a keyed non-empty one both pass through to the claim.
#[test]
fn only_a_keyed_empty_payload_is_refused_before_the_claim() {
    assert_eq!(
        empty_payload_refusal(Some("1:2:3"), true).as_deref(),
        Some("ERR usage: id= needs a non-empty payload\n")
    );
    assert_eq!(empty_payload_refusal(Some("1:2:3"), false), None);
    assert_eq!(empty_payload_refusal(None, true), None);
    assert_eq!(empty_payload_refusal(None, false), None);
}

use super::*;

fn nonce() -> LaunchNonce {
    LaunchNonce::generate()
}

/// A producer with an attempt in flight is refused, not displaced, so the running
/// attempt's outcome survives and a later release cannot reinstate an
/// unsettleable `Running` tip.
#[test]
fn a_running_producer_is_refused_rather_than_displaced() {
    const DUP: &str = "OK dup=1\n";
    let idem = PtyIdem::default();
    let a = Key {
        producer: 7,
        seq: 5,
    };
    let b = Key {
        producer: 7,
        seq: 9,
    };

    let running = match idem.claim(Realm::Local, a, DUP) {
        Claimed::Fresh(claim) => claim,
        Claimed::Answer(reply) => panic!("a fresh sequence must claim: {reply}"),
    };
    match idem.claim(Realm::Local, b, DUP) {
        Claimed::Answer(reply) => assert_eq!(
            reply, "ERR busy idem=5\n",
            "the refusal names the sequence actually in flight, not the caller's"
        ),
        Claimed::Fresh(_) => {
            panic!("a running mark was displaced: A can no longer settle its own outcome")
        }
    }

    running.applied();
    match idem.claim(Realm::Local, a, DUP) {
        Claimed::Answer(reply) => {
            assert_eq!(reply, DUP, "the replay of an applied seq is a dup")
        }
        Claimed::Fresh(_) => panic!("an applied sequence must not run twice"),
    }

    let b_claim = match idem.claim(Realm::Local, b, DUP) {
        Claimed::Fresh(claim) => claim,
        Claimed::Answer(reply) => panic!("B must proceed once A has settled: {reply}"),
    };
    b_claim.released();
    match idem.claim(Realm::Local, b, DUP) {
        Claimed::Fresh(claim) => claim.applied(),
        Claimed::Answer(reply) => {
            panic!("a released sequence is a FIRST attempt, not a duplicate: {reply}")
        }
    }
    match idem.claim(Realm::Local, a, DUP) {
        Claimed::Answer(reply) => assert_eq!(reply, DUP),
        Claimed::Fresh(_) => panic!("A's sequence is consumed"),
    }
}

#[test]
fn feed_idempotent_key_grammar_is_epoch_producer_seq() {
    let live = nonce();
    let hex = live.to_hex();
    assert_eq!(
        parse_key(&format!("{hex}:7:1"), live),
        Ok(Key {
            producer: 7,
            seq: 1
        })
    );
    let dead = nonce();
    assert_eq!(
        parse_key(&format!("{}:7:1", dead.to_hex()), live).unwrap_err(),
        EPOCH_REFUSAL
    );
    for bad in [
        String::from("nope"),
        format!("{hex}:7"),
        format!("{hex}:7:1:2"),
        format!("{hex}:x:1"),
        format!("{hex}:7:0"),
        format!("{hex}:7:-1"),
        format!("{}:7:1", &hex[..30]),
    ] {
        assert_eq!(
            parse_key(&bad, live).unwrap_err(),
            USAGE,
            "must not parse: {bad}"
        );
    }
}
