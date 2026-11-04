// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::age::Identity;
use crate::commons::binding::Request;
use crate::membership_log::{Body, Entrant, Inserted, Log, encode_records};

// The old byte-slice recovery algorithm, independent of recover_decoded.
fn legacy_recovery(bytes: &[u8]) -> Store {
    let mut store = Store::with_compiled(None);
    if store.ingest(bytes).is_ok() {
        return store;
    }
    let mut records = Vec::new();
    let mut at = 0;
    while at < bytes.len() && records.len() < MAX_RECORDS {
        let Ok(len) = Record::wire_len_prefix(&bytes[at..]) else {
            break;
        };
        let Some(end) = at.checked_add(len).filter(|&end| end <= bytes.len()) else {
            break;
        };
        if let Ok(record) = Record::decode(&bytes[at..end]) {
            records.push(record);
        }
        at = end;
    }
    for record in super::super::causal(records, |_| false) {
        let _ = store.insert(record);
    }
    store
}

struct Chunks<'a> {
    bytes: &'a [u8],
    at: usize,
    chunk: usize,
    fail_at: Option<usize>,
    largest: usize,
}

impl<'a> Chunks<'a> {
    fn new(bytes: &'a [u8], chunk: usize) -> Self {
        Self {
            bytes,
            at: 0,
            chunk,
            fail_at: None,
            largest: 0,
        }
    }
}

impl Read for Chunks<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.largest = self.largest.max(out.len());
        if self.fail_at == Some(self.at) {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        let before_error = self.fail_at.map_or(usize::MAX, |at| at - self.at);
        let n = out
            .len()
            .min(self.chunk)
            .min(self.bytes.len() - self.at)
            .min(before_error);
        out[..n].copy_from_slice(&self.bytes[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

fn fixture() -> Vec<Record> {
    let seed = [17; 32];
    let incarnation = [18; 16];
    let request = Request::create(
        [20; 16],
        [21; 32],
        [22; 16],
        incarnation,
        &Identity::from_secret_bytes([19; 32]),
        &seed,
    )
    .unwrap();
    let actor = request.binding().actor();
    let genesis = Record::found(
        &[23; 32],
        vec![Entrant {
            request,
            node_key: [24; 32],
            name: "reader".into(),
            label: "m1".into(),
        }],
        1,
    )
    .unwrap();
    let mut log = Log::new(genesis).unwrap();
    let name = |log: &Log, text: &str| {
        log.draft(&actor, 2, Body::Name { name: text.into() })
            .sign(&seed, incarnation)
            .unwrap()
    };
    let fork = name(&log, "fork");
    assert_eq!(log.insert(name(&log, "first")).unwrap(), Inserted::Held);
    assert_eq!(log.insert(fork).unwrap(), Inserted::Held);
    assert_eq!(log.insert(name(&log, "last")).unwrap(), Inserted::Held);
    let mut records: Vec<_> = log.records().cloned().collect();
    // A verified child whose parent is absent must remain waiting, not become complete.
    let parent = name(&log, "missing-parent");
    log.insert(parent).unwrap();
    records.push(name(&log, "waiting-child"));
    records
}

#[test]
fn layout_reader_keeps_unknown_large_frames_without_relaxing_known_reader() {
    let records = fixture();
    let known = &records[1];
    let record = crate::membership_log::Draft {
        network: *known.network_id(),
        sequence: known.sequence(),
        previous: known.previous().copied(),
        parents: known.parents().to_vec(),
        authored_ms: known.authored_ms(),
        body: Body::Put {
            audience: crate::membership_log::EpochAudience::Everyone,
            epoch: [42; 32],
            salt: [43; 32],
            ciphertext: vec![44; 80 * 1024].into(),
        },
    }
    .sign(&[17; 32], [18; 16])
    .unwrap();
    let bytes = crate::membership_log::with_kind_code(&record, 0xee);
    let mut small = Chunks::new(&bytes, 11);
    let mut layout = FrameReader::new();
    let frame = match layout.read_layout(&mut small, bytes.len()) {
        Ok(Some(frame)) => frame,
        _ => panic!("a complete unknown-kind layout must be framed"),
    };
    assert_eq!(frame.wire.as_slice(), bytes);
    assert_eq!(frame.parents, known.parents());
    assert_eq!(small.at, bytes.len());
    let mut known_reader = FrameReader::new();
    assert!(matches!(
        known_reader.read(&mut bytes.as_slice()),
        Err(FrameReadError::Invalid(_))
    ));
}

#[test]
fn layout_reader_checks_remaining_budget_before_reading_the_frame_body() {
    let record = fixture().remove(0);
    let header = Record::frame_header_len(record.bytes()).unwrap();
    let mut source = Chunks::new(record.bytes(), 7);
    source.fail_at = Some(header);
    let mut reader = FrameReader::new();
    assert!(matches!(
        reader.read_layout(&mut source, record.bytes().len() - 1),
        Err(FrameReadError::Invalid(_))
    ));
    assert_eq!(source.at, header, "an over-budget frame body was requested");
}

#[test]
fn rooted_file_application_rolls_back_only_new_unrooted_records() {
    let records = fixture();
    let Body::Genesis { signer, .. } = records[0].body() else {
        panic!("fixture genesis");
    };
    let existing = records[1].clone();
    let mut original = Store::dormant(&[[99; 32]]);
    original.insert(existing.clone()).unwrap();
    let original_bytes = original.encode();
    let original_network = original.network;
    for include_rejected_genesis in [false, true] {
        let bytes = encode_records(records.iter().enumerate().filter_map(|(i, record)| {
            (i != 1 && (include_rejected_genesis || i != 0)).then_some(record)
        }));
        let batch = Store::read_records(bytes.as_slice()).unwrap();
        assert_eq!(batch.bytes_read(), bytes.len());
        let recovered = batch.apply_rooted(original.clone());
        assert!(recovered.log().is_none());
        assert_eq!(recovered.encode(), original_bytes);
        assert_eq!(recovered.network, original_network);
        assert_eq!(recovered.keys(), Some([[99; 32]].as_slice()));
    }

    // The same formerly refused genesis can root the store after its signer is trusted.
    // The old unrooted record is absent from the new file and must still be promoted.
    let bytes = encode_records(
        records
            .iter()
            .enumerate()
            .filter_map(|(i, record)| (i != 1).then_some(record)),
    );
    let recovered = Store::read_records(bytes.as_slice())
        .unwrap()
        .apply_rooted(original.under(&[*signer]));
    assert!(recovered.log().is_some());
    assert_eq!(
        recovered.complete_bytes(existing.id()),
        Some(existing.bytes())
    );
    assert_eq!(recovered.keys(), Some([*signer].as_slice()));
}

#[test]
fn fragmented_recovery_preserves_exact_records_forks_and_waiting() {
    let records = fixture();
    let ordered = encode_records(&records);
    let reversed = encode_records(records.iter().rev());
    let mut bad_signature = records.clone();
    let mut bad = bad_signature[1].bytes().to_vec();
    *bad.last_mut().unwrap() ^= 1;
    let mut signature_stream = encode_records([&bad_signature.remove(0)]);
    signature_stream.extend_from_slice(&bad);
    signature_stream.extend_from_slice(&encode_records(&bad_signature[1..]));
    let mut unknown = ordered.clone();
    unknown[records[0].bytes().len() + crate::membership_log::KIND_AT] = 0xff;
    let mut garbage = ordered.clone();
    garbage.extend_from_slice(b"bad tail");
    let mut cases = vec![
        ordered.clone(),
        reversed,
        signature_stream,
        unknown,
        garbage,
    ];
    for n in [0, 1, FRAME_PREFIX_LEN - 1, ordered.len() - 1] {
        cases.push(ordered[..n].to_vec());
    }
    for bytes in cases {
        let expected = legacy_recovery(&bytes);
        for chunk in [1, 17, 257] {
            let mut input = Chunks::new(&bytes, chunk);
            let actual = Store::with_compiled(None)
                .recovering_read(&mut input)
                .unwrap();
            assert_eq!(actual.encode(), expected.encode());
            assert_eq!(actual.complete_inventory(), expected.complete_inventory());
            assert_eq!(actual.inventory(), expected.inventory());
            assert_eq!(
                input.at,
                bytes.len(),
                "recovery hashes even the ignored tail"
            );
        }
    }
}

#[test]
fn late_undo_and_original_epoch_wraps_survive_framed_replay() {
    let (before, after, epoch) = crate::membership_log::tests::history_reader_fixture();
    assert_eq!(before.keyed_epochs().len(), 1);
    assert!(after.keyed_epochs().is_empty());
    assert_ne!(before.membership(), after.membership());
    for log in [&before, &after] {
        for reversed in [false, true] {
            let mut records: Vec<_> = log.records().collect();
            if reversed {
                records.reverse();
            }
            let bytes = encode_records(records);
            let expected = legacy_recovery(&bytes);
            let mut input = Chunks::new(&bytes, 31);
            let actual = Store::with_compiled(None)
                .recovering_read(&mut input)
                .unwrap();
            assert_eq!(actual.encode(), expected.encode());
            let actual = actual.log().unwrap();
            assert_eq!(actual.membership(), log.membership());
            assert_eq!(actual.keyed_epochs(), log.keyed_epochs());
            assert_eq!(
                actual.get(&epoch).unwrap().bytes(),
                before.get(&epoch).unwrap().bytes()
            );
            for record in before
                .records()
                .filter(|r| matches!(r.body(), Body::Wrap { .. }))
            {
                assert_eq!(actual.get(record.id()).unwrap().bytes(), record.bytes());
            }
        }
    }
    let initial = legacy_recovery(&encode_records(before.records()));
    let delta = encode_records(after.records().filter(|r| before.get(r.id()).is_none()));
    let updated = initial.recovering_read(Chunks::new(&delta, 17)).unwrap();
    assert_eq!(updated.log().unwrap().membership(), after.membership());
    assert_eq!(updated.log().unwrap().keyed_epochs(), after.keyed_epochs());
}

#[test]
fn recovery_bound_counts_verified_frames_including_duplicates() {
    let records = fixture();
    let mut bad = records[0].bytes().to_vec();
    *bad.last_mut().unwrap() ^= 1;
    let mut bytes = bad;
    for _ in 0..MAX_RECORDS - 1 {
        bytes.extend_from_slice(records[0].bytes());
    }
    bytes.extend_from_slice(records[1].bytes());
    bytes.extend_from_slice(records[2].bytes());
    let expected = legacy_recovery(&bytes);
    let mut input = Chunks::new(&bytes, 257);
    let actual = Store::with_compiled(None)
        .recovering_read(&mut input)
        .unwrap();
    assert_eq!(actual.encode(), expected.encode());
    assert!(actual.complete_bytes(records[1].id()).is_some());
    assert!(actual.complete_bytes(records[2].id()).is_none());
    assert_eq!(input.at, bytes.len());
}

#[test]
fn actual_read_errors_are_not_torn_tail_recovery() {
    let records = fixture();
    let bytes = encode_records(&records);
    for fail_at in [0, records[0].bytes().len(), bytes.len()] {
        let mut input = Chunks::new(&bytes, 17);
        input.fail_at = Some(fail_at);
        assert!(Store::with_compiled(None).recovering_read(input).is_err());
    }
    let mut invalid_tail = records[0].bytes().to_vec();
    invalid_tail.resize(invalid_tail.len() + 512, 0);
    let mut input = Chunks::new(&invalid_tail, 17);
    input.fail_at = Some(records[0].bytes().len() + FRAME_PREFIX_LEN + 50);
    assert!(Store::with_compiled(None).recovering_read(input).is_err());
}

#[test]
fn oversized_frame_length_is_refused_before_body_allocation() {
    let records = fixture();
    let mut bytes = records[0].bytes().to_vec();
    let header_len = Record::frame_header_len(&bytes).unwrap();
    bytes[header_len - 4..header_len].copy_from_slice(&u32::MAX.to_be_bytes());
    let mut input = Chunks::new(&bytes, 17);
    let actual = Store::with_compiled(None)
        .recovering_read(&mut input)
        .unwrap();
    assert!(actual.is_empty());
    assert!(input.largest <= 16 * 1024);
    assert_eq!(input.at, bytes.len());
}
