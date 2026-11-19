// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Record version 2, shed frames and the `Checkpoint` kind (tracker FL-148a): read where the
//! format admits them, written by nothing but these tests.

use super::*;

const FORMAT_TWO: Reading = Reading::Format(crate::crossing::FORMAT_TWO);

/// Nine version-1 records, one of each kind, from fixed keys and fields.
fn golden_v1() -> Vec<Record> {
    let mac = |tag: u8| {
        let identity = crate::age::Identity::from_secret_bytes([tag ^ 0x55; 32]);
        let request = Request::create(
            [2; 16], [3; 32], [tag; 16], [tag; 16], &identity, &[tag; 32],
        )
        .unwrap();
        (identity, request)
    };
    let entrant = |tag: u8, label: &str| Entrant {
        request: mac(tag).1,
        node_key: [tag; 32],
        name: format!("mac-{tag}"),
        label: label.to_owned(),
    };
    let genesis = Record::found(&[99; 32], vec![entrant(1, "m1"), entrant(2, "m2")], 1).unwrap();
    let network = *genesis.id();
    let actor = mac(1).1.binding().actor();
    let mut out = vec![genesis];
    let mut previous: Option<RecordId> = None;
    let mut parent = network;
    let mut epoch = [0; 32];
    let node_pair = key_pair(&[0x42; 32]).unwrap();
    let node_key: [u8; 32] = node_pair.public_key().as_ref().try_into().unwrap();
    let mut proof = [0; 64];
    proof.copy_from_slice(
        node_pair
            .sign(&node_proof_message(&network, &actor))
            .as_ref(),
    );
    let mut sequence = 0;
    let mut sign = |body: Body, out: &mut Vec<Record>| {
        sequence += 1;
        let record = Draft {
            network,
            sequence,
            previous,
            parents: vec![parent],
            authored_ms: 1_700_000_000_000 + sequence,
            body,
        }
        .sign(&[1; 32], [1; 16])
        .unwrap();
        previous = Some(*record.id());
        parent = *record.id();
        out.push(record.clone());
        record
    };
    for body in [
        Body::Admit {
            entrant: entrant(3, "m3"),
            evidence: Evidence::Phrase,
        },
        Body::Remove {
            member: mac(2).1.binding().actor(),
            cutoff: network,
        },
        Body::Name {
            name: "golden".to_owned(),
        },
        Body::Epoch {
            audience: EpochAudience::Everyone,
            generation: 1,
            commitment: [7; 32],
        },
    ] {
        let record = sign(body, &mut out);
        if record.kind() == Kind::Epoch {
            epoch = *record.id();
        }
    }
    let recipient = mac(3).0.recipient();
    for body in [
        Body::Wrap {
            epoch,
            recipient,
            capsule: vec![5; 40].into(),
        },
        Body::Put {
            audience: EpochAudience::Hosts(vec!["m1".to_owned(), "m2".to_owned()]),
            epoch,
            salt: [9; 32],
            ciphertext: vec![0xc1; 48].into(),
        },
        Body::Delete {
            audience: EpochAudience::Everyone,
            epoch,
            salt: [8; 32],
            ciphertext: vec![0xd1; 20].into(),
        },
        Body::Node { node_key, proof },
    ] {
        sign(body, &mut out);
    }
    out
}

/// Each golden record's kind, length and id, as origin/main 57559cd0f21 (0.204) signed them
/// before record version 2 existed.
const GOLDEN: [(Kind, usize, &str); 9] = [
    (
        Kind::Genesis,
        720,
        "bb7e6d7647d34ff8b8c9f14c1e0bcd07f3c5f1bef962a75efc101ca308c29d49",
    ),
    (
        Kind::Admit,
        489,
        "8cad325851ae8f0a32215f1d8dc8a3e6dc8bcc1b10bbe37bf3cfbeb88ce14482",
    ),
    (
        Kind::Remove,
        315,
        "517ad4c60d5c865e9fbc1ba32918867d087a50416c2d4da06b63b7453cb6743a",
    ),
    (
        Kind::Name,
        242,
        "41f99bc960db649775fc39dd654068bb213012c8232f69227676648fa5cf32fc",
    ),
    (
        Kind::Epoch,
        276,
        "8000bdcc622a7d2b6ab701bc6475733263eecc6377b46d41c0c7dcebb3a2281c",
    ),
    (
        Kind::Wrap,
        341,
        "73e2eae94698cc5a338ae12918c2560fedcaa5e89db93f7cf206414dfd624326",
    ),
    (
        Kind::Put,
        359,
        "11bb2fa4588aaba9ae3c39af7217b33ec17df15091ce8da7336847aa8355f8f7",
    ),
    (
        Kind::Delete,
        322,
        "34c53d77773691404eacb1e8f293875d34425e7a783d704135ff41c1e03787e0",
    ),
    (
        Kind::Node,
        331,
        "78b13909809f8ec434b657d954ba302c53791fd05b53e2110bfa8e55580f0acc",
    ),
];

/// The golden `Put`'s whole bytes, as 0.204 signed them.
const GOLDEN_PUT: &str = "414c4d3101bb7e6d7647d34ff8b8c9f14c1e0bcd07f3c5f1bef962a75efc101ca308c29d49078a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c01010101010101010101010101010101000000000000000673e2eae94698cc5a338ae12918c2560fedcaa5e89db93f7cf206414dfd6243260173e2eae94698cc5a338ae12918c2560fedcaa5e89db93f7cf206414dfd6243260000018bcfe568060000007c0102026d31026d328000bdcc622a7d2b6ab701bc6475733263eecc6377b46d41c0c7dcebb3a2281c090909090909090909090909090909090909090909090909090909090909090900000030c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1ad8ef544120370e86e4bc959667e9104084948b1649dcbb37eff5ed4cd1f7b07645d338d72661f53f0dbeb626c612aec9fab543ef787f383f0f612424d8d6203";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Every version-1 record this build signs is byte for byte the one 0.204 signed, under the id
/// 0.204 gave it, and every reader (format 1, format 2, a follow copy) reads it back as it is,
/// as version 1 with no statement. NEGATIVE CONTROL: the fixtures were captured from 0.204's
/// code before any of record version 2 was written; a change to the version-1 header, body,
/// signing domain or id (writing the version byte from the wrong field, hashing the shed form of
/// a version-1 record) changes a length or an id here.
#[test]
fn vault_checkpoint_v1_records_are_byte_for_byte_unchanged() {
    let records = golden_v1();
    assert_eq!(records.len(), GOLDEN.len());
    for (record, (kind, len, id)) in records.iter().zip(GOLDEN) {
        let bytes = record.wire_bytes().unwrap();
        assert_eq!(
            (record.kind(), bytes.len(), hex(record.id())),
            (kind, len, id.to_owned())
        );
        assert_eq!(
            digest(&bytes),
            *record.id(),
            "a version-1 id is its bytes' SHA-256"
        );
        assert_eq!((record.version(), record.statement()), (1, None));
        assert!(!record.is_shed());
        for reading in [Reading::ONE, FORMAT_TWO, Reading::Follow] {
            let read = Record::decode_in(&bytes, reading).unwrap();
            assert_eq!(read.id(), record.id(), "{kind:?} at {reading:?}");
            assert_eq!(read.wire_bytes().unwrap(), bytes);
        }
    }
    let put = records.iter().find(|r| r.kind() == Kind::Put).unwrap();
    assert_eq!(hex(&put.wire_bytes().unwrap()), GOLDEN_PUT);
    #[cfg(unix)]
    assert_eq!(hex(put.frame().unwrap()), GOLDEN_PUT[..2 * (359 - 48 - 64)]);
    // No version-1 record of the checkpoint kind exists: a draft refuses to sign one.
    let checkpoint = Draft {
        network: *records[0].id(),
        sequence: 1,
        previous: None,
        parents: vec![*records[0].id()],
        authored_ms: 1,
        body: checkpoint_body(vec![*records[0].id()]),
    };
    assert!(checkpoint.sign(&[1; 32], [1; 16]).is_err());
}

fn checkpoint_body(heads: Vec<RecordId>) -> Body {
    Body::Checkpoint {
        predecessor: Some([0x44; 32]),
        cut_ms: 1_700_000_000_000,
        heads,
        state: [0x55; 32],
        dropped: [0x66; 32],
    }
}

/// A founded log, A's `[everyone]` epoch in it, and a draft by A that follows both.
fn log_with_epoch() -> (Log, RecordId) {
    let mut log = super::found(&[(super::A, "m1"), (super::B, "m2")]);
    let epoch = super::sign(
        &log,
        super::A,
        Body::Epoch {
            audience: EpochAudience::Everyone,
            generation: 1,
            commitment: [7; 32],
        },
    );
    let id = *epoch.id();
    assert_eq!(log.insert(epoch).unwrap(), Inserted::Held);
    (log, id)
}

fn v2_put(log: &Log, epoch: RecordId, statement: &Statement) -> Record {
    log.draft(
        &super::A.id(),
        9,
        Body::Put {
            audience: EpochAudience::Everyone,
            epoch,
            salt: [9; 32],
            ciphertext: vec![0xc2; 300].into(),
        },
    )
    .sign_two(&super::A.seed(), super::A.incarnation(), Some(statement))
    .unwrap()
}

fn statement() -> Statement {
    Statement {
        supersedes: vec![[1; 32], [2; 32], [3; 32]],
        carries: Some([2; 32]),
    }
}

/// The clear length's place in a version-2 record with `parents` parents.
fn lens_at(parents: usize) -> usize {
    header_len(parents, VERSION_TWO) - 8
}

/// A version-2 `Put` signs its clear fields and its ciphertext's length and SHA-256, so its
/// frame without the ciphertext still verifies, under the same id: the frame is what
/// `wire_bytes` and `wire_len` send for it, and its id is the SHA-256 of those bytes. Whichever
/// comes first, a whole record takes its frame's place in a log and a store, and a frame never
/// takes a whole record's. NEGATIVE CONTROLS: a ciphertext other than the one signed, a tail
/// length that is neither zero nor the signed length, and a statement changed in the frame are
/// each refused; a `Delete` that carries a value, a statement past 4,096 ids and a checkpoint
/// with a ciphertext are refused.
#[test]
fn vault_checkpoint_a_v2_put_verifies_after_its_ciphertext_is_shed() {
    let (mut log, epoch) = log_with_epoch();
    let whole = v2_put(&log, epoch, &statement());
    assert_eq!(whole.version(), 2);
    assert_eq!(whole.statement(), Some(&statement()));
    assert!(!whole.is_shed());
    let whole_bytes = whole.wire_bytes().unwrap().into_owned();
    assert_ne!(
        digest(&whole_bytes),
        *whole.id(),
        "a whole record's id is its shed form's"
    );

    let shed = whole.shed().unwrap();
    assert!(shed.is_shed());
    assert_eq!(shed.id(), whole.id());
    assert_eq!(shed.wire_len(), whole.wire_len() - 300);
    let shed_bytes = shed.wire_bytes().unwrap().into_owned();
    assert_eq!(shed_bytes.len(), shed.wire_len());
    assert_eq!(
        digest(&shed_bytes),
        *whole.id(),
        "a shed record's id is its bytes' SHA-256"
    );
    // The frame read back from its bytes, at format 2 and in a follow copy, verifies in full.
    for reading in [FORMAT_TWO, Reading::Follow] {
        let read = Record::decode_in(&shed_bytes, reading).unwrap();
        assert_eq!(read.id(), whole.id());
        assert!(read.is_shed());
        assert_eq!(read.statement(), Some(&statement()));
        let Body::Put {
            audience,
            epoch: read_epoch,
            salt,
            ciphertext,
        } = read.body()
        else {
            panic!("a Put");
        };
        assert_eq!(
            (audience, *read_epoch, *salt),
            (&EpochAudience::Everyone, epoch, [9; 32])
        );
        assert_eq!((ciphertext.len(), ciphertext.as_ref()), (300, &[][..]));
        assert!(ciphertext.is_shed());
        #[cfg(unix)]
        assert!(
            read.frame().is_none(),
            "a shed record has nothing to shelve"
        );
    }
    let Body::Put { ciphertext, .. } = whole.body() else {
        panic!("a Put");
    };
    assert_eq!(ciphertext.as_ref(), &[0xc2; 300][..]);
    #[cfg(unix)]
    {
        assert_eq!(
            whole.frame().unwrap(),
            &whole_bytes[..whole_bytes.len() - 300]
        );
        assert_eq!(whole_id(&whole_bytes), Some(*whole.id()));
        assert_eq!(whole_id(&shed_bytes), Some(*whole.id()));
        // What another crate reads a record's id by, whole and shed alike.
        assert_eq!(record_id(&whole_bytes), record_id(&shed_bytes));
        assert_eq!(record_id(&whole_bytes), Some(*whole.id()));
        assert_ne!(digest(&whole_bytes), *whole.id());
    }

    // NEGATIVE CONTROLS on the bytes.
    let mut other_ciphertext = whole_bytes.clone();
    *other_ciphertext.last_mut().unwrap() ^= 1;
    assert_eq!(
        Record::decode_in(&other_ciphertext, FORMAT_TWO)
            .unwrap_err()
            .to_string(),
        "a version-2 record's ciphertext is not the one it signs"
    );
    #[cfg(unix)]
    assert_eq!(whole_id(&other_ciphertext), None);
    let lens = lens_at(whole.parents().len());
    let mut short_tail = whole_bytes.clone();
    short_tail[lens + 4..lens + 8].copy_from_slice(&299u32.to_be_bytes());
    short_tail.pop();
    assert!(Record::decode_in(&short_tail, FORMAT_TWO).is_err());
    let mut other_statement = shed_bytes.clone();
    let carries = shed_bytes.len() - 64 - 32 - 4 - 32;
    other_statement[carries] ^= 1;
    assert_eq!(
        Record::decode_in(&other_statement, FORMAT_TWO)
            .unwrap_err()
            .to_string(),
        "membership record signature failed"
    );
    let delete = log
        .draft(
            &super::A.id(),
            9,
            Body::Delete {
                audience: EpochAudience::Everyone,
                epoch,
                salt: [8; 32],
                ciphertext: vec![0xd2; 20].into(),
            },
        )
        .sign_two(&super::A.seed(), super::A.incarnation(), Some(&statement()));
    assert!(
        delete.is_err(),
        "a Delete seals no value, so it carries none"
    );
    let too_many = Statement {
        supersedes: (0..=MAX_SUPERSEDES as u32)
            .map(|i| {
                let mut id = [1; 32];
                id[28..].copy_from_slice(&i.to_be_bytes());
                id
            })
            .collect(),
        carries: None,
    };
    let over = log
        .draft(
            &super::A.id(),
            9,
            Body::Put {
                audience: EpochAudience::Everyone,
                epoch,
                salt: [9; 32],
                ciphertext: vec![0xc2; 30].into(),
            },
        )
        .sign_two(&super::A.seed(), super::A.incarnation(), Some(&too_many));
    assert!(over.is_err());

    // Whole wins over frame, whichever reaches the log first.
    let mut framed = log.clone();
    assert_eq!(framed.insert(shed.clone()).unwrap(), Inserted::Held);
    let before = framed.held_bytes();
    assert_eq!(framed.insert(whole.clone()).unwrap(), Inserted::Known);
    assert!(!framed.get(whole.id()).unwrap().is_shed());
    assert_eq!(framed.held_bytes(), before + 300);
    assert_eq!(framed.insert(shed.clone()).unwrap(), Inserted::Known);
    assert!(
        !framed.get(whole.id()).unwrap().is_shed(),
        "a frame never takes a whole record's place"
    );
    assert_eq!(log.insert(whole.clone()).unwrap(), Inserted::Held);
    assert_eq!(framed.heads(), log.heads());
    // A merge brings the whole record to a log that holds every id but only the frame.
    let mut only_frame = log_with_epoch().0;
    only_frame.insert(shed.clone()).unwrap();
    only_frame.merge(&log).unwrap();
    assert!(!only_frame.get(whole.id()).unwrap().is_shed());
    // And a store, which answers that its bytes changed.
    let genesis = log.records().next().unwrap().clone();
    let mut store = Store::with_compiled(None);
    for record in [genesis, log.records().nth(1).unwrap().clone(), shed.clone()] {
        assert!(store.insert(record).unwrap());
    }
    assert!(store.insert(whole.clone()).unwrap());
    assert!(!store.insert(shed).unwrap());
    assert!(!store.insert(whole.clone()).unwrap());
    assert!(!store.log().unwrap().get(whole.id()).unwrap().is_shed());

    // A checkpoint is version 2 with no ciphertext; one given a tail is refused.
    let checkpoint = log
        .draft(&super::A.id(), 9, checkpoint_body(log.heads()))
        .sign_two(&super::A.seed(), super::A.incarnation(), None)
        .unwrap();
    let bytes = checkpoint.wire_bytes().unwrap().into_owned();
    assert_eq!(digest(&bytes), *checkpoint.id());
    let mut tailed = bytes.clone();
    let lens = lens_at(checkpoint.parents().len());
    tailed[lens + 4..lens + 8].copy_from_slice(&1u32.to_be_bytes());
    tailed.push(0);
    assert_eq!(
        Record::decode_in(&tailed, FORMAT_TWO)
            .unwrap_err()
            .to_string(),
        "a Checkpoint carries no ciphertext"
    );
}

/// A node of format 1 (every node of this build: nothing stores a format mark) refuses a
/// version-2 record, whole or shed, and a checkpoint, exactly as 0.204 does: one record over the
/// mesh is "not a membership record" ([`Record::decode`]), a stream that holds one is refused
/// whole ([`decode_records`]), a records file recovers what comes before it and stops there, the
/// streamed frame reader takes nothing past it, no prefix of one is a cut frame, and the head
/// reads none of it. A version-1 frame of the checkpoint's code is of an unknown kind at every
/// format, as it was. The same bytes read at format 2 and in a follow copy.
/// NEGATIVE CONTROL: a reader that admitted version 2 at format 1 ([`Reading::admits_two`]
/// true for `Format(1)`) decodes each record here at format 1 and fails the first assertion.
#[test]
fn vault_checkpoint_a_format_one_node_refuses_v2_as_0_204_does() {
    let (mut log, epoch) = log_with_epoch();
    let put = v2_put(&log, epoch, &statement());
    let shed = put.shed().unwrap();
    let checkpoint = log
        .draft(&super::A.id(), 9, checkpoint_body(log.heads()))
        .sign_two(&super::A.seed(), super::A.incarnation(), None)
        .unwrap();
    let genesis = log
        .records()
        .next()
        .unwrap()
        .wire_bytes()
        .unwrap()
        .into_owned();
    let epoch_bytes = log.get(&epoch).unwrap().wire_bytes().unwrap().into_owned();
    for record in [&put, &shed, &checkpoint] {
        let bytes = record.wire_bytes().unwrap().into_owned();
        for format_one in [Reading::ONE, Reading::Format(crate::crossing::FORMAT_ONE)] {
            // 0.204's `parse` and `frame_prefix_reader`: the magic, then a version byte that is
            // not 1.
            assert_eq!(
                Record::decode_in(&bytes, format_one)
                    .unwrap_err()
                    .to_string(),
                "not a membership record"
            );
            assert_eq!(
                Record::wire_len_prefix(&bytes, format_one)
                    .unwrap_err()
                    .to_string(),
                "not a membership record"
            );
            // 0.204's `is_cut_frame`: a version byte present and not 1 is no cut frame.
            for cut in MAGIC.len() + 1..=bytes.len() {
                assert!(
                    !Record::is_cut_frame(&bytes[..cut], format_one),
                    "cut at {cut}"
                );
            }
        }
        assert_eq!(
            Record::decode(&bytes).unwrap_err().to_string(),
            "not a membership record"
        );
        for reading in [FORMAT_TWO, Reading::Follow] {
            assert_eq!(
                Record::decode_in(&bytes, reading).unwrap().id(),
                record.id()
            );
            assert_eq!(
                Record::wire_len_prefix(&bytes, reading).unwrap(),
                bytes.len()
            );
            for cut in 1..bytes.len() {
                assert!(Record::is_cut_frame(&bytes[..cut], reading), "cut at {cut}");
            }
        }
        // The whole stream.
        let stream = [genesis.as_slice(), &epoch_bytes, &bytes].concat();
        assert_eq!(
            decode_records(&stream).unwrap_err().to_string(),
            "not a membership record"
        );
        assert!(store::decode_records_before_cut(&stream).is_err());
        assert_eq!(
            decode_records_in(&stream, MAX_HELD_RECORDS, FORMAT_TWO)
                .unwrap()
                .len(),
            3
        );
        // A records file recovers what precedes it, and stops there.
        let recovered = Store::with_compiled(None).recovering(&stream);
        assert_eq!(recovered.log().unwrap().held_count(), 2);
        let two = Store::with_compiled(None)
            .read_at(FORMAT_TWO)
            .recovering(&stream);
        assert_eq!(two.log().unwrap().held_count(), 3);
        #[cfg(unix)]
        {
            let read = Store::read_records(stream.as_slice(), MAX_HELD_RECORDS, Reading::ONE)
                .unwrap()
                .apply(Store::with_compiled(None));
            assert_eq!(read.log().unwrap().held_count(), 2);
            // A server whose mark names format 2 reads the file at it.
            let read = Store::read_records(stream.as_slice(), MAX_HELD_RECORDS, FORMAT_TWO)
                .unwrap()
                .apply(Store::with_compiled(None).read_at(FORMAT_TWO));
            assert_eq!(read.log().unwrap().held_count(), 3);
            // The head's reads.
            assert!(Record::vouched(&bytes, Reading::ONE).is_err());
            assert_eq!(
                Record::vouched(&bytes, FORMAT_TWO).unwrap().id(),
                record.id()
            );
        }
    }
    // A version-1 frame of the checkpoint's code: an unknown kind, at every format.
    let name = super::sign(
        &log,
        super::A,
        Body::Name {
            name: "x".to_owned(),
        },
    );
    log.insert(name.clone()).unwrap();
    let mut coded = name.wire_bytes().unwrap().into_owned();
    coded[KIND_AT] = Kind::Checkpoint.code();
    for reading in [Reading::ONE, FORMAT_TWO, Reading::Follow] {
        assert_eq!(
            Record::decode_in(&coded, reading).unwrap_err().to_string(),
            "membership record of an unknown kind"
        );
    }
    // A version-2 frame of a kind version 2 does not carry.
    let mut renamed = shed.wire_bytes().unwrap().into_owned();
    renamed[KIND_AT] = Kind::Name.code();
    assert!(Record::decode_in(&renamed, FORMAT_TWO).is_err());
    #[cfg(unix)]
    shelved_v2_reads_back_only_at_format_two(&put);
}

/// A version-2 `Put` shelved by its frame, as the head keeps one, reads back whole and checked
/// at format 2, and is refused at format 1 and where its file holds another ciphertext.
#[cfg(unix)]
fn shelved_v2_reads_back_only_at_format_two(put: &Record) {
    use std::io::Write as _;
    let dir = aterm_tempfile::tempdir().unwrap();
    let bytes = put.wire_bytes().unwrap().into_owned();
    let frame = put.frame().unwrap().to_vec();
    let mut tampered = bytes.clone();
    *tampered.last_mut().unwrap() ^= 1;
    let source = |bytes: &[u8], name: &str| {
        let path = dir.path().join(name);
        std::fs::File::create(&path)
            .unwrap()
            .write_all(bytes)
            .unwrap();
        Arc::new(payload::Source::new(
            std::fs::File::open(&path).unwrap(),
            bytes.len() as u64,
        ))
    };
    let shelve =
        |source, reading| Record::shelved(*put.id(), bytes.len(), &frame, 0, source, reading);
    assert!(shelve(source(&bytes, "one"), Reading::ONE).is_err());
    let shelved = shelve(source(&bytes, "two"), FORMAT_TWO).unwrap();
    assert!(shelved.is_shelved());
    assert_eq!(shelved.statement(), put.statement());
    assert_eq!(shelved.wire_bytes().unwrap(), bytes);
    let other = shelve(source(&tampered, "other"), FORMAT_TWO).unwrap();
    assert!(other.wire_bytes().is_err());
}

/// The checkpoint kind is in every kind table: named in [`Kind::ALL`] and by its word, counted
/// against the keyed budget, a membership-class record, and bounded by [`Kind::most_bytes`],
/// which a checkpoint of [`MAX_PARENTS`] parents and [`MAX_CHECKPOINT_HEADS`] heads meets
/// exactly. NEGATIVE CONTROLS: one head past the bound, no heads, unsorted heads and a zero
/// digest are each refused.
#[test]
fn vault_checkpoint_a_checkpoint_is_a_keyed_membership_record_within_its_bound() {
    assert_eq!(Kind::ALL.last(), Some(&Kind::Checkpoint));
    assert_eq!(Kind::Checkpoint.word(), "checkpoint");
    assert!(!Kind::Checkpoint.weighed());
    assert_eq!(
        budgets::Class::of_kind(Kind::Checkpoint),
        Some(budgets::Class::Membership)
    );
    assert_eq!(
        Kind::from_code(Kind::Checkpoint.code()).unwrap(),
        Kind::Checkpoint
    );
    let ids = |n: usize, base: u8| -> Vec<RecordId> {
        (0..n)
            .map(|i| {
                let mut id = [base; 32];
                id[28..].copy_from_slice(&u32::try_from(i).unwrap().to_be_bytes());
                id
            })
            .collect()
    };
    let parents = ids(MAX_PARENTS, 0x10);
    let draft = |heads: Vec<RecordId>, state: [u8; 32]| Draft {
        network: [0x20; 32],
        sequence: 2,
        previous: Some(parents[0]),
        parents: parents.clone(),
        authored_ms: 1,
        body: Body::Checkpoint {
            predecessor: Some([0x44; 32]),
            cut_ms: 1,
            heads,
            state,
            dropped: [0x66; 32],
        },
    };
    let sign = |draft: Draft| draft.sign_two(&[1; 32], [1; 16], None);
    let largest = sign(draft(ids(MAX_CHECKPOINT_HEADS, 0x30), [0x55; 32])).unwrap();
    assert_eq!(largest.wire_len(), Kind::Checkpoint.most_bytes());
    assert_eq!(
        budgets::counted(largest.kind()),
        RecordCounts {
            weighed: 0,
            keyed: 1
        }
    );
    assert!(sign(draft(ids(MAX_CHECKPOINT_HEADS + 1, 0x30), [0x55; 32])).is_err());
    assert!(sign(draft(Vec::new(), [0x55; 32])).is_err());
    let mut unsorted = ids(2, 0x30);
    unsorted.reverse();
    assert!(sign(draft(unsorted, [0x55; 32])).is_err());
    assert!(sign(draft(ids(1, 0x30), [0; 32])).is_err());
}

/// `count` distinct, sorted, non-zero ids.
fn ids_of(count: usize) -> Vec<RecordId> {
    (1..=count)
        .map(|i| {
            let mut id = [1; 32];
            id[28..].copy_from_slice(&u32::try_from(i).unwrap().to_be_bytes());
            id
        })
        .collect()
}

/// The shed frame of `draft` signed by A with `statement`, made as a writer that sheds before it
/// sends would make it: the signature never covers the tail length, so a frame needs no whole
/// record first.
fn frame_bytes(draft: &Draft, statement: &Statement) -> Vec<u8> {
    let pair = key_pair(&super::A.seed()).unwrap();
    let header = draft.header(VERSION_TWO, &pair, super::A.incarnation());
    let (clear, _) = encode_clear(&draft.body, Some(statement)).unwrap();
    let mut wire = encode_header(&header);
    put_len(&mut wire, clear.len(), 4).unwrap();
    let lens = wire.len() - 4;
    put_len(&mut wire, 0, 4).unwrap();
    wire.extend_from_slice(&clear);
    let signature = pair.sign(&signed_two(&wire, lens, wire.len()));
    wire.extend_from_slice(signature.as_ref());
    wire
}

/// A shed frame is read only where its whole record would be: the bytes through its signature
/// and the ciphertext length it signs fit [`MAX_RECORD`], so no signed id stands for a record no
/// reader takes whole. A `Put` of the largest ciphertext with the most supersedes that fit is
/// read whole and shed; one more id and its frame is refused as its whole record is, though the
/// statement is within [`MAX_SUPERSEDES`] and the ciphertext within its own bound.
/// NEGATIVE CONTROL: without the bound in `decode_clear` the over-long frame decodes, and this
/// failed with "a frame whose whole record passes MAX_RECORD is not read: Ok(Record { id: …".
#[test]
fn vault_checkpoint_a_frame_is_read_only_where_its_whole_record_fits() {
    let (log, epoch) = log_with_epoch();
    let draft = log.draft(
        &super::A.id(),
        9,
        Body::Put {
            audience: EpochAudience::Everyone,
            epoch,
            salt: [9; 32],
            ciphertext: vec![0xc2; entries::MAX_PUT_CIPHERTEXT].into(),
        },
    );
    let statement = |count: usize| Statement {
        supersedes: ids_of(count),
        carries: None,
    };
    let bare = frame_bytes(&draft, &statement(0)).len();
    let fits = (MAX_RECORD - bare - entries::MAX_PUT_CIPHERTEXT) / 32;
    assert!(
        fits < MAX_SUPERSEDES,
        "CONTROL: the two bounds together pass the record's ({fits})"
    );

    let most = statement(fits);
    let whole = draft
        .clone()
        .sign_two(&super::A.seed(), super::A.incarnation(), Some(&most));
    let whole = whole.unwrap();
    assert!(whole.wire_len() <= MAX_RECORD);
    let frame = frame_bytes(&draft, &most);
    assert_eq!(frame.len() + entries::MAX_PUT_CIPHERTEXT, whole.wire_len());
    for reading in [FORMAT_TWO, Reading::Follow] {
        let read = Record::decode_in(&frame, reading).unwrap();
        assert!(read.is_shed());
        assert_eq!(read.id(), whole.id());
    }

    let over = statement(fits + 1);
    assert_eq!(
        draft
            .clone()
            .sign_two(&super::A.seed(), super::A.incarnation(), Some(&over))
            .unwrap_err()
            .to_string(),
        "membership record is too long"
    );
    let frame = frame_bytes(&draft, &over);
    assert!(frame.len() < MAX_RECORD);
    for reading in [FORMAT_TWO, Reading::Follow] {
        let read = Record::decode_in(&frame, reading);
        assert_eq!(
            read.as_ref().map_err(ToString::to_string).err().as_deref(),
            Some("membership record is too long"),
            "a frame whose whole record passes MAX_RECORD is not read: {read:?}"
        );
    }
    // A format-1 node refuses it as it refuses every version-2 record.
    assert_eq!(
        Record::decode_in(&frame, Reading::ONE)
            .unwrap_err()
            .to_string(),
        "not a membership record"
    );
}

/// A genesis-rooted store holding the founding records and A's epoch of `log`.
fn store_of(log: &Log) -> Store {
    let mut store = Store::with_compiled(None);
    for record in log.records().take(2) {
        assert!(store.insert(record.clone()).unwrap());
    }
    store
}

/// One batch that brings a version-2 record both whole and shed keeps the whole one, in either
/// order: a whole record always wins over its frame, inside a batch as across two.
/// NEGATIVE CONTROL: `causal` keeping the last of each id held the frame for `[whole, frame]`,
/// and this failed with "the whole record wins in a batch of [whole, frame]".
#[test]
fn vault_checkpoint_one_batch_keeps_the_whole_record_over_its_frame() {
    let (log, epoch) = log_with_epoch();
    let whole = v2_put(&log, epoch, &statement());
    let shed = whole.shed().unwrap();
    for (name, batch) in [
        ("[whole, frame]", vec![whole.clone(), shed.clone()]),
        ("[frame, whole]", vec![shed.clone(), whole.clone()]),
    ] {
        let mut store = store_of(&log);
        assert_eq!(store.ingest_records(batch.clone()).unwrap(), 1, "{name}");
        let held = store.log().unwrap().get(whole.id()).unwrap();
        assert!(
            !held.is_shed(),
            "the whole record wins in a batch of {name}"
        );
        let ordered = store::causal(batch, |_| false);
        assert_eq!(ordered.len(), 1, "{name}");
        assert!(!ordered[0].is_shed(), "{name}");
    }
}

/// A whole record that takes the place of a frame a store held counts as a record that entered
/// it, and the records file that held the frame is no prefix of the store: the store is written
/// whole, not as an append of nothing past the file, which would leave the frame on file and
/// lose the whole record at the next read.
/// NEGATIVE CONTROLS: counting by ids alone ingested 0 records ("a whole record that took its
/// frame's place entered the store": left 0, right 1), and `extends` comparing ids alone
/// answered `Some(3)` ("the file of the frame is no prefix of the store that holds the whole").
#[test]
fn vault_checkpoint_a_whole_record_that_takes_its_frames_place_is_written_whole() {
    let (log, epoch) = log_with_epoch();
    let whole = v2_put(&log, epoch, &statement());
    let shed = whole.shed().unwrap();
    let mut on_file = store_of(&log);
    assert!(on_file.insert(shed.clone()).unwrap());
    let mut store = on_file.clone();
    assert_eq!(
        store.ingest_records(vec![whole.clone()]).unwrap(),
        1,
        "a whole record that took its frame's place entered the store"
    );
    assert!(!store.log().unwrap().get(whole.id()).unwrap().is_shed());
    assert_eq!(store.inventory(), on_file.inventory());
    #[cfg(unix)]
    {
        assert_eq!(
            store.extends(&on_file),
            None,
            "the file of the frame is no prefix of the store that holds the whole"
        );
        // CONTROL: a store that only grew past the file still appends.
        let mut grown = on_file.clone();
        let name = super::sign(&log, super::B, Body::Name { name: "b".into() });
        assert_eq!(grown.ingest_records(vec![name]).unwrap(), 1);
        assert_eq!(grown.extends(&on_file), Some(3));
    }
    // Again: the frame adds nothing, and the whole record nothing more.
    assert_eq!(store.ingest_records(vec![shed]).unwrap(), 0);
    assert_eq!(store.ingest_records(vec![whole]).unwrap(), 0);
}

/// Taking a whole record in its frame's place lets go of what the log derived from the frame's
/// bytes, and the generation history leases are taken under names the bytes, not only the ids:
/// a log holding the frame and one holding the whole record differ, and a log that took the
/// whole record has the generation of one that held it from the start.
/// NEGATIVE CONTROLS: a generation of the ids alone made the frame's log and the whole's equal
/// ("a frame and its whole record are different bytes"), and `take_whole` keeping the cached
/// generation kept the frame's ("taking the whole record lets go of the frame's generation").
#[cfg(unix)]
#[test]
fn vault_checkpoint_taking_the_whole_record_lets_go_of_what_the_frame_derived() {
    let generation = |log: &Log| {
        history::HistoryView::new(log)
            .lease_complete()
            .unwrap()
            .generation()
    };
    let (log, epoch) = log_with_epoch();
    let whole = v2_put(&log, epoch, &statement());
    let shed = whole.shed().unwrap();
    let mut held_whole = log.clone();
    held_whole.insert(whole.clone()).unwrap();
    let mut framed = log.clone();
    framed.insert(shed).unwrap();
    let of_frame = generation(&framed);
    assert_ne!(
        of_frame,
        generation(&held_whole),
        "a frame and its whole record are different bytes"
    );
    assert_eq!(framed.insert(whole).unwrap(), Inserted::Known);
    assert_eq!(
        generation(&framed),
        generation(&held_whole),
        "taking the whole record lets go of the frame's generation"
    );
}
