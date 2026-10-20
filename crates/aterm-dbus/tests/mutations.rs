// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The decoder on damaged input: every recording, mutated many times from a
//! fixed seed (overwritten, truncated, spliced, its length words stomped).
//! Nothing panics, and what is accepted is accepted for what it is:
//!
//! * a value is accepted only as its own canonical encoding (it re-encodes to
//!   exactly the bytes read);
//! * a message decodes and re-encodes, and that encoding decodes and
//!   re-encodes to the same bytes (bytes, not `==`: a NaN double is never
//!   equal to itself, so value equality would fail a message that is fine);
//! * `frame_len` answers every prefix.
//!
//! The cargo-fuzz target under `fuzz/` holds the same properties without the
//! seed (a STOCK EXCEPTION lane; see its header).

use crate::support::*;
use aterm_dbus::message::{self, Message};
use aterm_dbus::{Endian, Type, wire};
use aterm_spec::rng::XorShift;

const SEED: u64 = 0x0A11_7DB0;

/// One damaged copy of `bytes`.
fn mutate(rng: &mut XorShift, bytes: &[u8]) -> Vec<u8> {
    let mut out = bytes.to_vec();
    for _ in 0..rng.range(1, 4) {
        match rng.below(6) {
            0 if !out.is_empty() => {
                let i = rng.index(out.len());
                out[i] = rng.next_u64() as u8;
            }
            1 if !out.is_empty() => out.truncate(rng.index(out.len())),
            2 => {
                let i = rng.index(out.len() + 1);
                let n = rng.range(1, 9);
                let junk: Vec<u8> = (0..n).map(|_| rng.next_u64() as u8).collect();
                out.splice(i..i, junk);
            }
            3 if out.len() >= 4 => {
                // A length word set to an edge: zero, one, the array and
                // message limits and past them, all ones.
                let at = rng.index(out.len() / 4) * 4;
                let edges = [
                    0u32,
                    1,
                    0x0400_0000,
                    0x0400_0001,
                    0x0800_0000,
                    0x7fff_ffff,
                    u32::MAX,
                ];
                let v = edges[rng.index(edges.len())];
                out[at..at + 4].copy_from_slice(&v.to_le_bytes());
            }
            4 if out.len() >= 2 => {
                let a = rng.index(out.len());
                let b = rng.range(a, out.len());
                let copy = out[a..b].to_vec();
                out.extend_from_slice(&copy);
            }
            _ => {}
        }
    }
    out
}

/// `m`, decoded from `bytes`, re-encodes, and its encoding is a fixed point.
fn assert_round_trip(m: &Message, bytes: &[u8]) {
    let again = m
        .encode()
        .unwrap_or_else(|e| panic!("accepted but not encodable ({e}): {}", hex(bytes)));
    let twice = Message::decode(&again)
        .and_then(|m| m.encode())
        .unwrap_or_else(|e| panic!("its encoding does not round-trip ({e}): {}", hex(bytes)));
    assert_eq!(hex(&twice), hex(&again), "{}", hex(bytes));
}

/// The cargo-fuzz target's first find (crash-c593292b3b372145, 6.6 M runs into
/// the review's run): `get_items_reply` damaged so its body signature opens
/// `d` where it had `a`, and the array's length word and padding read as a
/// NaN double. The decoder was right; the harness judged it by `==`.
const NAN_REPLY: &str = "6c020001040100000c00000036000000050175004e00000006017300040000003a312e39000000000801670018642828736f2928736f2928736f2969696173737573617529000000fc000000faffffff050000003a312e34320000001c0000002f6f72672f613131792f61747370692f61636365737369626c652f3700000000050000003a312e34320000001f0000002f6f72672f613131792f61747370692f61636365737369626c652f726f6f7400050000003a312e34320000001f0000002f6f72672f613131792f61747370692f61636365737369626c652f726f6f7400000000000200000038000000190000006f72672e613131792e61747370692e41636365737369626c65000000130000006f72672e613131792e61747370692e546578740000000000000000002b00280008000000540f726d696e616c00000000080000000240000000000000";

#[test]
fn a_nan_double_round_trips_by_its_bits() {
    let bytes = unhex(NAN_REPLY);
    let m = Message::decode(&bytes).expect("the fuzzer's input decodes");
    let nan = m.body.first().and_then(aterm_dbus::Value::as_f64);
    assert!(nan.is_some_and(f64::is_nan), "{:?}", m.body.first());
    // The control: value equality cannot judge this message, even against
    // itself, which is why the round trip compares bytes.
    assert_ne!(m, m.clone());
    assert_round_trip(&m, &bytes);
    let bits = nan.map(f64::to_bits);
    let again = Message::decode(&m.encode().unwrap()).unwrap();
    assert_eq!(
        again
            .body
            .first()
            .and_then(aterm_dbus::Value::as_f64)
            .map(f64::to_bits),
        bits
    );
}

fn messages() -> Vec<Vec<u8>> {
    let mut all: Vec<Vec<u8>> = rows("zbus-5.16.0-messages.tsv")
        .iter()
        .map(|r| unhex(&r[1]))
        .collect();
    all.extend(
        rows("zbus-5.16.0-session.tsv")
            .iter()
            .filter(|r| r[2] == "msg")
            .map(|r| unhex(&r[3])),
    );
    all
}

#[test]
fn a_damaged_message_decodes_to_itself_or_is_refused() {
    let mut rng = XorShift::new(SEED);
    let mut accepted = 0;
    let mut tries = 0;
    for original in messages() {
        for _ in 0..1500 {
            let bytes = mutate(&mut rng, &original);
            tries += 1;
            for end in [0, 1, 15, 16, 17, bytes.len()] {
                let _ = message::frame_len(&bytes[..end.min(bytes.len())]);
            }
            let Ok(m) = Message::decode(&bytes) else {
                continue;
            };
            accepted += 1;
            assert_round_trip(&m, &bytes);
        }
    }
    // Some damage lands where the message survives it (a flag bit, a byte of
    // a string): the round trip above is exercised, not vacuous.
    assert!(
        accepted > 100 && accepted < tries,
        "{accepted} of {tries} accepted"
    );
}

#[test]
fn a_damaged_value_is_refused_or_canonical() {
    let mut rng = XorShift::new(SEED ^ 1);
    let mut accepted = 0;
    for r in rows("zvariant-5.12.0-values.tsv") {
        let ty = [Type::parse(&r[1]).unwrap()];
        let endian = if r[2] == "l" {
            Endian::Little
        } else {
            Endian::Big
        };
        let offset: usize = r[3].parse().unwrap();
        let original = unhex(&r[4]);
        for _ in 0..300 {
            let bytes = mutate(&mut rng, &original);
            if let Ok(values) = wire::decode(&bytes, &ty, endian, offset) {
                accepted += 1;
                assert_eq!(
                    wire::encode(&values, endian, offset).unwrap(),
                    bytes,
                    "{} {}",
                    r[0],
                    hex(&bytes)
                );
            }
        }
    }
    assert!(accepted > 100, "{accepted} accepted");
}

#[test]
fn random_bytes_under_any_signature_never_panic() {
    let mut rng = XorShift::new(SEED ^ 2);
    let sigs = [
        "v",
        "av",
        "a{sv}",
        "(so)(so)(so)iiassusau",
        "aay",
        "a(ua(so))",
        "g",
        "o",
        "s",
        "b",
        "d",
        "aaaav",
    ];
    for sig in sigs {
        let types = aterm_dbus::sig::parse_signature(sig).unwrap();
        for _ in 0..3000 {
            let n = rng.index(64);
            let bytes: Vec<u8> = (0..n).map(|_| rng.next_u64() as u8 & 0x8f).collect();
            let _ = wire::decode(&bytes, &types, Endian::Little, rng.index(8));
        }
    }
}
