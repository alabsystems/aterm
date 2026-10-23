// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A tape's items come back from their bytes exactly.

use std::io::ErrorKind;

use astream_effects::EffectRecord;
use aterm_spec::rng::XorShift;

use super::super::doors;
use super::*;

/// `items` encoded by one encoder and decoded by one decoder, as a file's
/// frames are.
fn round_trip(items: &[Item]) -> Vec<Item> {
    let mut enc = Encoder::default();
    let mut bytes = Vec::new();
    for item in items {
        enc.item(item, &mut bytes);
    }
    let mut dec = Decoder::default();
    let mut input = Input::new(&bytes);
    let mut back = Vec::new();
    while !input.is_empty() {
        back.push(
            dec.item(&mut input)
                .expect("the encoder's own bytes decode"),
        );
    }
    back
}

/// The bytes of `items`, encoded in order.
fn encoded(enc: &mut Encoder, items: &[Item]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for item in items {
        enc.item(item, &mut bytes);
    }
    bytes
}

/// Text from `rng`: ASCII, a newline, a NUL, multi-byte characters.
fn text(rng: &mut XorShift, max: usize) -> String {
    const PIECES: [&str; 9] = ["a", "Z", " ", "\n", "\0", "é", "猫", "🐈", "@"];
    (0..rng.index(max + 1))
        .map(|_| *rng.pick(&PIECES))
        .collect()
}

/// Bytes from `rng`, any value.
fn bytes(rng: &mut XorShift, max: usize) -> Vec<u8> {
    (0..rng.index(max + 1))
        .map(|_| (rng.next_u64() & 0xff) as u8)
        .collect()
}

/// A path: under a root (as a header's roots spell one), a root alone, or
/// verbatim — one that itself starts with `@` included.
fn path(rng: &mut XorShift) -> String {
    match rng.index(6) {
        0 => format!("@ledger/{}", text(rng, 12)),
        1 => "@journal".to_string(),
        2 => format!("@@{}", text(rng, 6)),
        3 => "@".to_string(),
        4 => format!("@/{}", text(rng, 4)),
        _ => format!("/abs/{}", text(rng, 12)),
    }
}

/// A screen-sized body that shares most of itself with `base`.
fn near(rng: &mut XorShift, base: &[u8]) -> Vec<u8> {
    let mut body = base.to_vec();
    if body.is_empty() {
        return bytes(rng, 600);
    }
    let at = rng.index(body.len());
    let cut = rng.index(body.len() - at + 1);
    body.splice(at..at + cut, bytes(rng, 40));
    body
}

/// A clock value: small steps, steps back, and the extremes.
fn clock(rng: &mut XorShift, prev: u64) -> u64 {
    match rng.index(5) {
        0 => 0,
        1 => u64::MAX,
        2 => prev.wrapping_sub(rng.below(1_000)),
        _ => prev.wrapping_add(rng.below(5_000_000)),
    }
}

/// One generated item of every kind the codec carries.
fn item(rng: &mut XorShift, last: &mut (u64, u64, Vec<u8>)) -> Item {
    const DOORS: [&str; 5] = [
        doors::CTL,
        doors::TELLER,
        "host.at_idle",
        doors::OUT,
        "probe.git_view",
    ];
    let record = match rng.index(12) {
        0 => {
            last.0 = clock(rng, last.0);
            EffectRecord::Clock(last.0)
        }
        1 => EffectRecord::Rand(rng.next_u64()),
        2 => EffectRecord::File {
            path: path(rng),
            bytes: bytes(rng, 300),
        },
        3 => EffectRecord::FileErr {
            path: path(rng),
            kind: rng.pick(&KINDS).1,
        },
        4 => {
            last.1 = clock(rng, last.1);
            EffectRecord::Mono(last.1)
        }
        5 | 6 => {
            last.2 = near(rng, &last.2);
            EffectRecord::Exchange {
                door: (*rng.pick(&DOORS)).to_string(),
                request: ctl_request(&["text", "--json", "tail=40"]),
                reply: last.2.clone(),
            }
        }
        7 => EffectRecord::Emit {
            door: (*rng.pick(&DOORS)).to_string(),
            bytes: if rng.index(2) == 0 {
                last.2.clone()
            } else {
                bytes(rng, 80)
            },
        },
        8 => EffectRecord::Raised {
            door: doors::RAISED_STOP.to_string(),
            false_reads: if rng.index(2) == 0 {
                rng.below(40)
            } else {
                u64::MAX
            },
        },
        9 => {
            return Item::Note {
                kind: text(rng, 8),
                text: text(rng, 60),
            };
        }
        10 => {
            return Item::Mark(if rng.index(2) == 0 {
                Mark::Nested {
                    inner: "probe.git_view".to_string(),
                    outer: (*rng.pick(&DOORS)).to_string(),
                }
            } else {
                Mark::Truncated {
                    door: rng.next_u64(),
                }
            });
        }
        _ => {
            return Item::End(match rng.index(3) {
                0 => End::Returned { how: text(rng, 30) },
                1 => End::Suspended,
                _ => End::Unwound {
                    message: text(rng, 30),
                },
            });
        }
    };
    let stamp = (rng.index(2) == 0).then(|| Stamp {
        mono_ns: clock(rng, last.1),
        wall_ms: match rng.index(4) {
            0 => i64::MIN,
            1 => i64::MAX,
            2 => -1,
            _ => 1_790_000_000_000 + rng.below(100_000) as i64,
        },
    });
    Item::Effect { record, stamp }
}

/// Generated runs of items of every kind come back exactly, many seeds over.
#[test]
fn every_item_comes_back_from_its_bytes() {
    let mut kinds_seen = std::collections::BTreeSet::new();
    for seed in 1..=200u64 {
        let mut rng = XorShift::new(seed);
        let mut last = (0, 0, bytes(&mut rng, 3_000));
        let items: Vec<Item> = (0..rng.range(1, 80))
            .map(|_| item(&mut rng, &mut last))
            .collect();
        for i in &items {
            kinds_seen.insert(match i {
                Item::Effect { record, .. } => record.kind(),
                Item::Note { .. } => "Note",
                Item::Mark(_) => "Mark",
                Item::End(_) => "End",
            });
        }
        assert_eq!(round_trip(&items), items, "seed {seed}");
    }
    let every = [
        "Clock", "Rand", "File", "Mono", "Exchange", "Emit", "Raised", "Note", "Mark", "End",
    ];
    assert!(
        every.iter().all(|k| kinds_seen.contains(k)),
        "the generator reaches every kind: {kinds_seen:?}"
    );
}

/// A varint and a zigzag at their edges.
#[test]
fn integers_come_back_at_their_edges() {
    for v in [0, 1, 127, 128, 16_383, 16_384, u64::MAX - 1, u64::MAX] {
        let mut out = Vec::new();
        put_varint(&mut out, v);
        let mut input = Input::new(&out);
        assert_eq!(input.varint(), Ok(v));
        assert!(input.is_empty(), "{v}: no byte left over");
    }
    for v in [i64::MIN, i64::MIN + 1, -65, -64, -1, 0, 1, 63, 64, i64::MAX] {
        let mut out = Vec::new();
        put_signed(&mut out, v);
        assert_eq!(Input::new(&out).signed(), Ok(v));
    }
    let mut one = Vec::new();
    put_signed(&mut one, -1);
    assert_eq!(one.len(), 1, "a small step back is one byte");
    let over = [0xffu8, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x02];
    assert!(Input::new(&over).varint().is_err());
}

/// Every kind in the table comes back as itself; a kind the table does not
/// hold is written as `Other`, and a code this build does not know reads as
/// `Other`.
#[test]
fn every_error_kind_in_the_table_comes_back() {
    for (code, kind) in KINDS {
        assert_eq!(kind_code(kind), code, "{kind:?}");
        assert_eq!(kind_of(code), kind, "{code}");
    }
    assert_eq!(kind_of(250), ErrorKind::Other);
}

/// A screen read again unchanged is one byte of body; a screen whose last
/// rows changed is its changed middle; a short reply is whole.
#[test]
fn a_repeated_screen_is_one_byte_and_a_changed_one_its_middle() {
    let screen: Vec<u8> = (0..3_000).map(|i| b'a' + (i % 26) as u8).collect();
    let read = |reply: &[u8]| {
        Item::effect(EffectRecord::Exchange {
            door: doors::CTL.to_string(),
            request: ctl_request(&["text", "--json", "tail=40"]),
            reply: reply.to_vec(),
        })
    };
    let mut enc = Encoder::default();
    let first = encoded(&mut enc, &[read(&screen)]);
    assert!(first.len() > screen.len(), "the first read is whole");
    let again = encoded(&mut enc, &[read(&screen)]);
    let request_len = ctl_request(&["text", "--json", "tail=40"]).len();
    assert_eq!(again.len(), 1 + 1 + 1 + request_len + 1, "{again:?}");
    let mut changed = screen.clone();
    changed[2_900..2_910].copy_from_slice(b"0123456789");
    let delta = encoded(&mut enc, &[read(&changed)]);
    assert!(delta.len() < 40, "a splice: {} bytes", delta.len());
    let other = Item::effect(EffectRecord::Exchange {
        door: doors::CTL.to_string(),
        request: ctl_request(&["status"]),
        reply: screen.clone(),
    });
    assert!(encoded(&mut enc, &[other]).len() > screen.len());
    let items = [read(&screen), read(&screen), read(&changed), read(&screen)];
    assert_eq!(round_trip(&items), items);
}

/// The table remembers 64 keys, and a key used again — a repeat looked up, or
/// a short body that only replaces it — is the most recently used: the keys
/// that come after push out the least recently used, which are not the oldest
/// inserted. On both sides alike.
#[test]
fn the_table_forgets_its_least_recently_used_key() {
    let body: Vec<u8> = (0..1_000).map(|i| (i % 251) as u8).collect();
    let short: Vec<u8> = (0..200).map(|i| (i % 7) as u8 + b'a').collect();
    let emit = |door: &str, bytes: &[u8]| {
        Item::effect(EffectRecord::Emit {
            door: door.to_string(),
            bytes: bytes.to_vec(),
        })
    };
    let mut items: Vec<Item> = (0..delta::KEYS)
        .map(|i| emit(&format!("d{i}"), &body))
        .collect();
    items.push(emit("d0", &body));
    items.push(emit("d1", &short));
    items.push(emit("d64", &body));
    items.push(emit("d65", &body));
    let mut enc = Encoder::default();
    encoded(&mut enc, &items);
    assert!(encoded(&mut enc, &[emit("d0", &body)]).len() < 8);
    let longer = [short.as_slice(), &body].concat();
    assert!(encoded(&mut enc, &[emit("d1", &longer)]).len() < longer.len());
    assert!(encoded(&mut enc, &[emit("d2", &body)]).len() > body.len());
    items.push(emit("d0", &body));
    items.push(emit("d1", &longer));
    items.push(emit("d2", &body));
    assert_eq!(round_trip(&items), items);
}

/// A body over the table's limit is stored against the base its key holds,
/// but never becomes one: its key is forgotten, so the same body again is
/// whole, and so is the next after that.
#[test]
fn a_body_past_the_tables_limit_is_spliced_but_never_remembered() {
    let small: Vec<u8> = (0..1_000).map(|i| (i % 251) as u8).collect();
    let big = [small.as_slice(), &vec![7u8; delta::BODY_MAX]].concat();
    assert!(big.len() > delta::BODY_MAX);
    let emit = |bytes: &[u8]| {
        Item::effect(EffectRecord::Emit {
            door: doors::OUT.to_string(),
            bytes: bytes.to_vec(),
        })
    };
    let mut enc = Encoder::default();
    encoded(&mut enc, &[emit(&small)]);
    let spliced = encoded(&mut enc, &[emit(&big)]);
    assert!(
        spliced.len() < big.len() - small.len() + 16,
        "stored against the remembered body: {} bytes",
        spliced.len()
    );
    assert!(encoded(&mut enc, &[emit(&big)]).len() > big.len());
    assert!(encoded(&mut enc, &[emit(&small)]).len() > small.len());
    let items = [emit(&small), emit(&big), emit(&big), emit(&small)];
    assert_eq!(round_trip(&items), items);
}

/// A name costs its spelling once a file, and a byte after that.
#[test]
fn a_name_is_spelled_once_per_file() {
    let raised = || {
        Item::effect(EffectRecord::Raised {
            door: doors::RAISED_STOP.to_string(),
            false_reads: 3,
        })
    };
    let mut enc = Encoder::default();
    let first = encoded(&mut enc, &[raised()]);
    let second = encoded(&mut enc, &[raised()]);
    assert_eq!(second.len(), 3, "tag, door number, count: {second:?}");
    assert!(first.len() > doors::RAISED_STOP.len());
}

/// The control lane's request and reply forms come back, a transport error
/// and an empty screen included.
#[test]
fn the_control_lanes_forms_come_back() {
    for args in [
        vec![],
        vec!["text", "--json", "tail=40"],
        vec!["send", "é\n\0"],
    ] {
        assert_eq!(
            ctl_args(&ctl_request(&args)).expect("decodes"),
            args.iter().map(|a| (*a).to_string()).collect::<Vec<_>>()
        );
    }
    for reply in [
        Ok(CtlReply {
            code: 0,
            stdout: "{\"rows\":[]}".to_string(),
            stderr: String::new(),
        }),
        Ok(CtlReply {
            code: -75,
            stdout: String::new(),
            stderr: "aterm-ctl: ERR exited".to_string(),
        }),
        Ok(CtlReply {
            code: i32::MIN,
            stdout: "🐈".to_string(),
            stderr: "\n".to_string(),
        }),
        Err("server closed the connection".to_string()),
        Err(String::new()),
    ] {
        assert_eq!(ctl_reply_of(&ctl_reply(&reply)), Ok(reply.clone()));
    }
    assert!(ctl_args(&[2, 1, b'a']).is_err(), "a word missing");
    assert!(ctl_args(&[0, 9]).is_err(), "a byte after the last word");
    assert!(ctl_reply_of(&[7]).is_err(), "no reply form");
}

/// Two screens of different lengths share the reply's leading bytes up to
/// the screen's first difference: the stdout is last and unprefixed.
#[test]
fn two_replies_share_their_prefix_up_to_the_screens_difference() {
    let reply = |stdout: &str| {
        ctl_reply(&Ok(CtlReply {
            code: 0,
            stdout: stdout.to_string(),
            stderr: String::new(),
        }))
    };
    let a = reply("{\"rows\":[\"same top row\",\"short\"]}");
    let b = reply("{\"rows\":[\"same top row\",\"a much longer last row\"]}");
    let shared = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    assert!(shared > 20, "{shared}");
}

/// Bytes the encoder never writes are refused with where: an unknown tag, a
/// name the file never spelled, a repeat or a splice with no base, a length
/// past the frame, text that is not UTF-8.
#[test]
fn bytes_the_encoder_did_not_write_are_refused() {
    let refused = |bytes: &[u8]| {
        let mut input = Input::new(bytes);
        Decoder::default().item(&mut input).expect_err("refused")
    };
    assert_eq!(refused(&[0]).at, 0, "tag 0 is no item");
    assert_eq!(refused(&[0x7f]).at, 0, "nor is a tag past the last");
    refused(&[8, 6, 0]);
    refused(&[7, 0, 1, b'x', 1]);
    refused(&[7, 0, 1, b'x', 2, 1, 1, 0]);
    refused(&[3, 0, 1, b'/', 9, 1]);
    refused(&[9, 0, 1, b'k', 1, 0xff]);
    refused(&[11 | 0x80, 0, 0, 1]);
    let body = vec![b'q'; 300];
    let mut enc = Encoder::default();
    let mut bytes = encoded(
        &mut enc,
        &[Item::effect(EffectRecord::Emit {
            door: doors::OUT.to_string(),
            bytes: body,
        })],
    );
    bytes.extend([7, 1, 2]);
    put_varint(&mut bytes, 200);
    put_varint(&mut bytes, 200);
    bytes.push(0);
    let mut dec = Decoder::default();
    let mut input = Input::new(&bytes);
    dec.item(&mut input).expect("the stored body");
    assert!(dec.item(&mut input).is_err(), "a splice past its base");
}
