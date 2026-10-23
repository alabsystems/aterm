// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for `EventLog` encoding, decoding, truncation, seal, and range checks.

use astream_wire::frame::Frame;
use astream_wire::steps::{STEP_VERSION, encode_step};
use aterm_aos::log::{
    EventLog, IdentityMismatch, LOG_MAGIC, LogFault, LogFull, LogHeader, RECORD_VERSION,
    RangeFault, Tail,
};
use aterm_aos::task::Schema;
use aterm_aos::wire::{Enc, WireFault};
use aterm_aos::{ArtifactId, Digest, TaskIdentity};

use crate::toy::{ToyInput, ToySchema, Variant, identity};

type ToyLog = EventLog<ToySchema>;

/// The ancestor checkpoint every sample log extends.
fn ancestor() -> Digest {
    Digest::of(b"log-codec-test-ancestor\0", &[b"C0"])
}

fn header(first_cursor: u64) -> LogHeader {
    LogHeader {
        identity: identity(Variant::Good),
        log_epoch: 3,
        first_cursor,
        ancestor: ancestor(),
    }
}

fn sample_inputs() -> Vec<ToyInput> {
    vec![
        ToyInput::Add(1),
        ToyInput::Tick,
        ToyInput::Emit {
            target: 4,
            op: 2,
            payload: b"hi".to_vec(),
        },
        ToyInput::Reset,
        ToyInput::Add(-3),
        ToyInput::Tick,
    ]
}

fn log_of(inputs: &[ToyInput]) -> ToyLog {
    let mut log = EventLog::new(header(10), 64);
    for i in inputs {
        log.push(i.clone()).unwrap();
    }
    log
}

fn sample() -> (ToyLog, Vec<u8>) {
    let log = log_of(&sample_inputs());
    let bytes = log.encode(&ToySchema::new()).unwrap();
    (log, bytes)
}

fn decode(bytes: &[u8]) -> Result<(ToyLog, Tail), LogFault> {
    EventLog::decode(
        &ToySchema::new(),
        bytes,
        &identity(Variant::Good),
        &ancestor(),
        64,
    )
}

fn record(step: u64, body: &[u8]) -> Vec<u8> {
    encode_step(step, body).unwrap()
}

fn header_body(magic: [u8; 8], id: &TaskIdentity, first_cursor: u64, broken: u8) -> Vec<u8> {
    let mut e = Enc::new();
    e.u8(RECORD_VERSION);
    e.u8(0);
    e.raw(&magic);
    e.identity(id);
    e.u64(3);
    e.u64(first_cursor);
    e.digest(&ancestor());
    e.u8(broken);
    e.finish()
}

/// A seal record body naming `count` events and `digest`.
fn seal_body(count: u64, digest: &Digest) -> Vec<u8> {
    let mut e = Enc::new();
    e.u8(RECORD_VERSION);
    e.u8(2);
    e.u64(count);
    e.digest(digest);
    e.finish()
}

/// The length of a seal record (every seal record has the same length).
fn seal_len() -> usize {
    log_of(&[]).encode(&ToySchema::new()).unwrap().len() - good_header().len()
}

fn event_body(version: u8, kind: u8, input: &ToyInput) -> Vec<u8> {
    let mut e = Enc::new();
    e.u8(version);
    e.u8(kind);
    ToySchema::new().encode_input(input, &mut e);
    e.finish()
}

fn good_header() -> Vec<u8> {
    record(0, &header_body(LOG_MAGIC, &identity(Variant::Good), 10, 0))
}

#[test]
fn a_log_round_trips_with_a_clean_tail() {
    let (log, bytes) = sample();
    let (back, tail) = decode(&bytes).unwrap();
    assert_eq!(tail, Tail::Clean);
    assert_eq!(back.header(), log.header());
    assert_eq!(back.inputs(), log.inputs());
    assert_eq!(back.broken(), None);
    assert_eq!(back.end_cursor(), 16);
    assert_eq!(back.encode(&ToySchema::new()).unwrap(), bytes);
}

#[test]
fn a_broken_log_round_trips_broken_and_answers_no_range() {
    let (mut log, _) = sample();
    log.mark_broken("an input could not be recorded");
    let bytes = log.encode(&ToySchema::new()).unwrap();
    let (back, _) = decode(&bytes).unwrap();
    assert!(back.broken().is_some());
    assert!(matches!(back.range(10, 16), Err(RangeFault::Broken(_))));
}

#[test]
fn every_byte_flip_of_a_log_is_refused_or_reported_torn_never_misread() {
    let (log, bytes) = sample();
    let (mut torn, mut refused) = (0, 0);
    for at in 0..bytes.len() {
        for mask in [0x01u8, 0x80, 0xff] {
            let mut bad = bytes.clone();
            bad[at] ^= mask;
            match decode(&bad) {
                Ok((back, Tail::Torn { at: cut, dropped })) => {
                    torn += 1;
                    assert_eq!(cut + dropped, bad.len());
                    assert_eq!(back.header(), log.header());
                    assert_eq!(back.inputs(), log.inputs(), "byte {at} mask {mask:#x}");
                }
                Ok((_, tail)) => panic!("byte {at} mask {mask:#x} decoded {tail:?}"),
                Err(_) => refused += 1,
            }
        }
    }
    assert!(torn > 0 && refused > 0, "torn {torn}, refused {refused}");
}

/// Every proper prefix of a log's bytes is a refusal, a torn tail or a cut reported
/// `Unsealed` (a record-boundary cut after `k` of `n` events, the seal gone); only the whole
/// bytes, ending in the seal, decode `Clean`.
#[test]
fn every_truncation_of_a_log_is_its_prefix_reported_torn_or_unsealed_or_a_refusal() {
    let inputs = sample_inputs();
    let schema = ToySchema::new();
    let seal = seal_len();
    let bounds: Vec<usize> = (0..=inputs.len())
        .map(|k| log_of(&inputs[..k]).encode(&schema).unwrap().len() - seal)
        .collect();
    let bytes = log_of(&inputs).encode(&schema).unwrap();
    assert_eq!(bytes.len(), bounds[inputs.len()] + seal);
    let (mut unsealed, mut torn) = (0, 0);
    for cut in 0..bytes.len() {
        let got = decode(&bytes[..cut]);
        if cut < bounds[0] {
            let want = if cut < 2 {
                LogFault::NotALog
            } else {
                LogFault::MissingHeader
            };
            assert_eq!(got.err(), Some(want), "cut {cut}");
            continue;
        }
        let k = bounds.iter().rposition(|b| *b <= cut).unwrap();
        let (back, tail) = got.unwrap();
        assert_eq!(back.inputs(), &inputs[..k], "cut {cut}");
        let want = if cut == bounds[k] {
            unsealed += 1;
            Tail::Unsealed { events: k }
        } else {
            torn += 1;
            Tail::Torn {
                at: bounds[k],
                dropped: cut - bounds[k],
            }
        };
        assert_eq!(tail, want, "cut {cut}");
    }
    assert_eq!(unsealed, inputs.len() + 1, "every record-boundary cut");
    assert!(torn > 0);
    let (whole, tail) = decode(&bytes).unwrap();
    assert_eq!((whole.inputs(), tail), (&inputs[..], Tail::Clean));
}

/// The seal is the event count and the whole log's trace digest; a seal naming another count
/// or another digest is refused with both values, and a broken log is sealed like any other.
#[test]
fn a_seal_names_its_event_count_and_digest_and_a_seal_naming_others_is_refused() {
    let schema = ToySchema::new();
    let (log, bytes) = sample();
    let n = log.len() as u64;
    let digest = log.trace_digest(&schema, 10, 16).unwrap();
    let unsealed = &bytes[..bytes.len() - seal_len()];
    let resealed = |count: u64, d: &Digest| {
        let mut b = unsealed.to_vec();
        b.extend(record(n + 1, &seal_body(count, d)));
        b
    };
    assert_eq!(
        resealed(n, &digest),
        bytes,
        "the seal is count and trace digest"
    );
    assert_eq!(
        decode(&resealed(n - 1, &digest)).err(),
        Some(LogFault::SealCount {
            sealed: n - 1,
            found: n
        })
    );
    let other = log.trace_digest(&schema, 10, 15).unwrap();
    assert_eq!(
        decode(&resealed(n, &other)).err(),
        Some(LogFault::SealDigest {
            sealed: other,
            found: digest
        })
    );
    let (mut broken, _) = sample();
    broken.mark_broken("lost");
    let (back, tail) = decode(&broken.encode(&schema).unwrap()).unwrap();
    assert_eq!(tail, Tail::Clean);
    assert!(back.broken().is_some());
}

/// A seal can stand only last: one before an event, or a second seal, is a record of the
/// wrong kind at its step.
#[test]
fn a_seal_anywhere_but_last_is_refused_as_a_record_of_the_wrong_kind() {
    let schema = ToySchema::new();
    let one = log_of(&[ToyInput::Tick]);
    let sealed_one = one.encode(&schema).unwrap();
    let digest = one.trace_digest(&schema, 10, 11).unwrap();
    let mut then_event = sealed_one.clone();
    then_event.extend(record(3, &event_body(RECORD_VERSION, 1, &ToyInput::Tick)));
    assert_eq!(
        decode(&then_event).err(),
        Some(LogFault::Kind { at: 2, kind: 2 })
    );
    let mut two_seals = sealed_one;
    two_seals.extend(record(3, &seal_body(1, &digest)));
    assert_eq!(
        decode(&two_seals).err(),
        Some(LogFault::Kind { at: 2, kind: 2 })
    );
    let seal_first = record(0, &seal_body(0, &digest));
    assert_eq!(
        decode(&seal_first).err(),
        Some(LogFault::Kind { at: 0, kind: 2 })
    );
}

/// The header binds the log to its ancestor checkpoint: read beside another checkpoint, the
/// log is refused naming both, though its identity, epoch and first cursor are those the
/// reader expects.
#[test]
fn a_log_read_beside_another_checkpoint_is_refused_naming_both() {
    let (_, bytes) = sample();
    let other = Digest::of(b"log-codec-test-ancestor\0", &[b"C1"]);
    let got = EventLog::<ToySchema>::decode(
        &ToySchema::new(),
        &bytes,
        &identity(Variant::Good),
        &other,
        64,
    );
    assert_eq!(
        got.err(),
        Some(LogFault::Ancestor {
            expected: other,
            found: ancestor()
        })
    );
}

#[test]
fn damage_followed_by_a_whole_record_is_refused_not_truncated() {
    let (_, bytes) = sample();
    let header_len = good_header().len();
    let mut bad = bytes.clone();
    bad[header_len + 20] ^= 0x40;
    assert!(matches!(decode(&bad), Err(LogFault::Step(_))));
}

#[test]
fn a_header_under_another_identity_names_the_artifact_that_differs() {
    let (_, bytes) = sample();
    let good = identity(Variant::Good);
    let other = |a: &ArtifactId| ArtifactId {
        version: a.version + 1,
        ..a.clone()
    };
    let cases = [
        (
            TaskIdentity {
                task: other(&good.task),
                ..good.clone()
            },
            IdentityMismatch::Task,
        ),
        (
            TaskIdentity {
                code: other(&good.code),
                ..good.clone()
            },
            IdentityMismatch::Code,
        ),
        (
            TaskIdentity {
                contract: other(&good.contract),
                ..good.clone()
            },
            IdentityMismatch::Contract,
        ),
        (
            TaskIdentity {
                schema: other(&good.schema),
                ..good.clone()
            },
            IdentityMismatch::Schema,
        ),
        (
            TaskIdentity {
                profile: other(&good.profile),
                ..good.clone()
            },
            IdentityMismatch::Profile,
        ),
    ];
    for (expect, which) in cases {
        let got = EventLog::decode(&ToySchema::new(), &bytes, &expect, &ancestor(), 64);
        assert_eq!(got.err(), Some(LogFault::Identity(which)));
        assert_eq!(IdentityMismatch::first(&good, &expect), Some(which));
    }
    assert_eq!(IdentityMismatch::first(&good, &good), None);
}

#[test]
fn a_log_with_more_events_than_the_reader_accepts_is_refused() {
    let (_, bytes) = sample();
    let read = |limit| {
        EventLog::<ToySchema>::decode(
            &ToySchema::new(),
            &bytes,
            &identity(Variant::Good),
            &ancestor(),
            limit,
        )
    };
    assert_eq!(read(5).err(), Some(LogFault::TooManyEvents { limit: 5 }));
    assert!(read(6).is_ok());
}

#[test]
fn bytes_that_are_not_a_log_are_refused_as_not_a_log() {
    assert_eq!(decode(&[]).err(), Some(LogFault::NotALog));
    assert_eq!(decode(b"hello, world").err(), Some(LogFault::NotALog));
    let mut magic = LOG_MAGIC;
    magic[3] = b'X';
    let bytes = record(0, &header_body(magic, &identity(Variant::Good), 10, 0));
    assert_eq!(decode(&bytes).err(), Some(LogFault::NotALog));
}

#[test]
fn a_log_of_another_format_is_refused_by_its_number() {
    assert_eq!(LOG_MAGIC[7], 2, "format 2: the ancestor and the seal");
    for format in [1u8, 3] {
        let mut magic = LOG_MAGIC;
        magic[7] = format;
        let bytes = record(0, &header_body(magic, &identity(Variant::Good), 10, 0));
        assert_eq!(decode(&bytes).err(), Some(LogFault::Format(format)));
    }
}

#[test]
fn a_record_of_another_version_is_refused_by_its_number() {
    let mut bytes = good_header();
    bytes.extend(record(1, &event_body(9, 1, &ToyInput::Tick)));
    assert_eq!(decode(&bytes).err(), Some(LogFault::RecordVersion(9)));
    let mut head = header_body(LOG_MAGIC, &identity(Variant::Good), 10, 0);
    head[0] = 7;
    assert_eq!(
        decode(&record(0, &head)).err(),
        Some(LogFault::RecordVersion(7))
    );
}

#[test]
fn a_log_whose_first_record_is_an_event_has_no_header() {
    let bytes = record(0, &event_body(RECORD_VERSION, 1, &ToyInput::Tick));
    assert_eq!(decode(&bytes).err(), Some(LogFault::MissingHeader));
}

#[test]
fn a_record_of_a_kind_that_cannot_stand_at_its_step_is_refused_naming_the_step() {
    let mut bytes = good_header();
    bytes.extend(record(1, &event_body(RECORD_VERSION, 1, &ToyInput::Tick)));
    bytes.extend(record(2, &event_body(RECORD_VERSION, 7, &ToyInput::Tick)));
    assert_eq!(
        decode(&bytes).err(),
        Some(LogFault::Kind { at: 2, kind: 7 })
    );

    let mut second_header = good_header();
    second_header.extend(record(
        1,
        &header_body(LOG_MAGIC, &identity(Variant::Good), 10, 0),
    ));
    assert_eq!(
        decode(&second_header).err(),
        Some(LogFault::Kind { at: 1, kind: 0 })
    );

    let mut head = header_body(LOG_MAGIC, &identity(Variant::Good), 10, 0);
    head[1] = 5;
    assert_eq!(
        decode(&record(0, &head)).err(),
        Some(LogFault::Kind { at: 0, kind: 5 })
    );
}

#[test]
fn a_record_whose_frame_holds_bytes_past_its_body_is_refused() {
    let body = event_body(RECORD_VERSION, 1, &ToyInput::Tick);
    let mut payload = vec![STEP_VERSION];
    payload.extend_from_slice(&1u64.to_le_bytes());
    payload.extend_from_slice(&(body.len() as u32).to_le_bytes());
    payload.extend_from_slice(&body);
    payload.extend_from_slice(&[0, 0, 0]);
    let mut bytes = good_header();
    bytes.extend(Frame::new(payload).encode().unwrap());
    assert_eq!(
        decode(&bytes).err(),
        Some(LogFault::Wire(WireFault::Trailing { extra: 3 }))
    );
}

#[test]
fn an_event_body_that_is_not_one_canonical_input_is_refused() {
    let mut trailing = good_header();
    let mut body = event_body(RECORD_VERSION, 1, &ToyInput::Add(5));
    body.push(0xEE);
    trailing.extend(record(1, &body));
    assert_eq!(
        decode(&trailing).err(),
        Some(LogFault::Wire(WireFault::Trailing { extra: 1 }))
    );

    let mut bad_tag = good_header();
    bad_tag.extend(record(1, &[RECORD_VERSION, 1, 9]));
    assert_eq!(
        decode(&bad_tag).err(),
        Some(LogFault::Wire(WireFault::BadTag {
            field: "toy.input",
            tag: 9
        }))
    );
}

#[test]
fn a_broken_flag_other_than_zero_or_one_is_a_bad_tag() {
    let bytes = record(0, &header_body(LOG_MAGIC, &identity(Variant::Good), 10, 2));
    assert_eq!(
        decode(&bytes).err(),
        Some(LogFault::Wire(WireFault::BadTag {
            field: "log.broken",
            tag: 2
        }))
    );
}

#[test]
fn steps_out_of_order_are_refused_as_a_step_fault() {
    let mut gap = good_header();
    gap.extend(record(2, &event_body(RECORD_VERSION, 1, &ToyInput::Tick)));
    assert!(matches!(decode(&gap), Err(LogFault::Step(_))));
    let mut repeat = good_header();
    repeat.extend(record(1, &event_body(RECORD_VERSION, 1, &ToyInput::Tick)));
    repeat.extend(record(1, &event_body(RECORD_VERSION, 1, &ToyInput::Tick)));
    assert!(matches!(decode(&repeat), Err(LogFault::Step(_))));
}

#[test]
fn a_log_whose_cursors_would_leave_the_u64_range_is_refused() {
    let mut bytes = record(
        0,
        &header_body(LOG_MAGIC, &identity(Variant::Good), u64::MAX, 0),
    );
    bytes.extend(record(1, &event_body(RECORD_VERSION, 1, &ToyInput::Tick)));
    assert_eq!(
        decode(&bytes).err(),
        Some(LogFault::Wire(WireFault::TooLong {
            field: "log.events",
            len: 1,
            limit: 0
        }))
    );
}

#[test]
fn a_record_too_large_for_a_step_record_is_refused_at_encode() {
    let payload = vec![0u8; 16 << 20];
    let log = log_of(&[ToyInput::Emit {
        target: 1,
        op: 1,
        payload,
    }]);
    assert_eq!(
        log.encode(&ToySchema::new()).err(),
        Some(LogFault::TooLarge {
            at: 1,
            len: (16 << 20) + 13,
            limit: (16 << 20) - 13
        })
    );
}

#[test]
fn push_returns_each_cursor_and_refuses_past_max_events_leaving_the_log_untouched() {
    let mut log: ToyLog = EventLog::new(header(10), 2);
    assert_eq!(log.push(ToyInput::Tick), Ok(10));
    assert_eq!(log.push(ToyInput::Reset), Ok(11));
    assert_eq!(log.push(ToyInput::Add(1)), Err(LogFull { limit: 2 }));
    assert_eq!(log.inputs(), &[ToyInput::Tick, ToyInput::Reset]);
    assert_eq!(log.end_cursor(), 12);
}

#[test]
fn a_log_at_the_end_of_the_cursor_space_refuses_the_push_that_would_overflow() {
    let mut log: ToyLog = EventLog::new(header(u64::MAX - 1), 8);
    assert_eq!(log.push(ToyInput::Tick), Ok(u64::MAX - 1));
    assert_eq!(log.push(ToyInput::Tick), Err(LogFull { limit: 8 }));
    assert_eq!(log.end_cursor(), u64::MAX);
    assert_eq!(log.len(), 1);
}

#[test]
fn a_range_outside_the_log_is_refused_with_its_exact_fault() {
    let (mut log, _) = sample();
    let inputs = sample_inputs();
    assert_eq!(log.range(10, 16), Ok(&inputs[..]));
    assert_eq!(log.range(12, 14), Ok(&inputs[2..4]));
    assert_eq!(log.range(16, 16), Ok(&inputs[6..]));
    assert_eq!(
        log.range(14, 12),
        Err(RangeFault::Inverted { from: 14, to: 12 })
    );
    assert_eq!(
        log.range(9, 12),
        Err(RangeFault::BeforeStart { from: 9, first: 10 })
    );
    assert_eq!(
        log.range(10, 17),
        Err(RangeFault::PastEnd { to: 17, end: 16 })
    );
    assert_eq!(
        log.range(u64::MAX - 1, u64::MAX),
        Err(RangeFault::PastEnd {
            to: u64::MAX,
            end: 16
        })
    );
    log.mark_broken("lost");
    log.mark_broken("second reason");
    assert_eq!(log.broken(), Some("lost"));
    assert_eq!(log.range(10, 16), Err(RangeFault::Broken("lost")));
    assert_eq!(
        log.trace_digest(&ToySchema::new(), 10, 16),
        Err(RangeFault::Broken("lost"))
    );
}

#[test]
fn the_trace_digest_binds_the_inputs_their_order_the_range_the_epoch_and_the_ancestor() {
    let schema = ToySchema::new();
    let (log, _) = sample();
    let base = log.trace_digest(&schema, 10, 16).unwrap();
    assert_eq!(log.clone().trace_digest(&schema, 10, 16).unwrap(), base);
    assert_ne!(log.trace_digest(&schema, 10, 15).unwrap(), base);
    let mut reordered = sample_inputs();
    reordered.swap(0, 4);
    assert_ne!(
        log_of(&reordered).trace_digest(&schema, 10, 16).unwrap(),
        base
    );
    let mut other_epoch = EventLog::new(
        LogHeader {
            log_epoch: 4,
            ..header(10)
        },
        64,
    );
    for i in sample_inputs() {
        other_epoch.push(i).unwrap();
    }
    assert_ne!(other_epoch.trace_digest(&schema, 10, 16).unwrap(), base);
    let mut other_ancestor = EventLog::new(
        LogHeader {
            ancestor: Digest::of(b"log-codec-test-ancestor\0", &[b"C1"]),
            ..header(10)
        },
        64,
    );
    for i in sample_inputs() {
        other_ancestor.push(i).unwrap();
    }
    assert_ne!(other_ancestor.trace_digest(&schema, 10, 16).unwrap(), base);
}
