// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Independent byte-judge oracle for the production decoded receive path.

use super::*;
use crate::membership_log::store::{Frame, frames};
use crate::membership_log::untaken::{Noted, Sent, Untaken};
use crate::membership_log::{Body, Draft, Entrant, Evidence, MemberId, Record, RecordId, Store};
use std::collections::BTreeMap;
use std::io::{self, Read};

struct Fragments<'a> {
    bytes: &'a [u8],
    at: usize,
    reads: usize,
    fail_at: Option<usize>,
}

impl<'a> Fragments<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            at: 0,
            reads: 0,
            fail_at: None,
        }
    }
}

impl Read for Fragments<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        if self.fail_at == Some(self.at) {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        self.reads += 1;
        if self.reads.is_multiple_of(5) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let until_error = self
            .fail_at
            .map_or(usize::MAX, |at| at.saturating_sub(self.at));
        let count = out
            .len()
            .min(17)
            .min(self.bytes.len() - self.at)
            .min(until_error);
        out[..count].copy_from_slice(&self.bytes[self.at..self.at + count]);
        self.at += count;
        Ok(count)
    }
}

fn entrant(tag: u8) -> ([u8; 32], [u8; 16], MemberId, Entrant) {
    let writer = [tag; 32];
    let incarnation = [tag + 16; 16];
    let identity = Identity::from_secret_bytes([tag + 32; 32]);
    let request = crate::commons::binding::Request::create(
        [1; 16],
        [2; 32],
        [tag; 16],
        incarnation,
        &identity,
        &writer,
    )
    .unwrap();
    let actor = request.binding().actor();
    (
        writer,
        incarnation,
        actor,
        Entrant {
            request,
            node_key: [tag + 64; 32],
            name: format!("member-{tag}"),
            label: format!("m{tag}"),
        },
    )
}

struct History {
    store: Store,
    genesis: Record,
    writer: [u8; 32],
    incarnation: [u8; 16],
}

impl History {
    fn new() -> Self {
        let (writer, incarnation, _, founder) = entrant(3);
        let genesis = Record::found(&[4; 32], vec![founder], 1).unwrap();
        let mut store = Store::for_vault("team").unwrap();
        store.insert(genesis.clone()).unwrap();
        Self {
            store,
            genesis,
            writer,
            incarnation,
        }
    }

    fn name(&self, parents: Vec<RecordId>, previous: Option<RecordId>, sequence: u64) -> Record {
        Draft {
            network: *self.genesis.id(),
            sequence,
            previous,
            parents,
            authored_ms: 1_000 + sequence,
            body: Body::Name {
                name: format!("name-{sequence}"),
            },
        }
        .sign(&self.writer, self.incarnation)
        .unwrap()
    }
}

fn join(parts: &[&[u8]]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for part in parts {
        bytes.extend_from_slice(part);
    }
    bytes
}

// This is the selector before readable_mask was extracted. It deliberately does not call
// the shared selector, so a changed closure can disagree with this oracle.
fn legacy_readable(bytes: &[u8], frames: &[Frame]) -> Vec<u8> {
    let at: BTreeMap<RecordId, usize> = frames
        .iter()
        .enumerate()
        .map(|(i, frame)| (frame.id, i))
        .collect();
    let mut children: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (i, frame) in frames.iter().enumerate() {
        for parent in &frame.parents {
            if let Some(&j) = at.get(parent)
                && j != i
            {
                children.entry(j).or_default().push(i);
            }
        }
    }
    let mut unread: Vec<bool> = frames
        .iter()
        .map(|frame| {
            bytes
                .get(frame.range.clone())
                .is_none_or(|wire| Record::decode(wire).is_err())
        })
        .collect();
    let mut stack: Vec<usize> = (0..unread.len()).filter(|&i| unread[i]).collect();
    while let Some(j) = stack.pop() {
        for &i in children.get(&j).into_iter().flatten() {
            if !unread[i] {
                unread[i] = true;
                stack.push(i);
            }
        }
    }
    frames
        .iter()
        .zip(unread)
        .filter(|(_, unread)| !*unread)
        .filter_map(|(frame, _)| bytes.get(frame.range.clone()))
        .flatten()
        .copied()
        .collect()
}

fn legacy_judge(store: &Store, bytes: &[u8]) -> (Sent, bool, Option<Vec<u8>>) {
    let valid =
        |bytes: &[u8]| store.clone().ingest(bytes).is_ok() || Store::decode("team", bytes).is_ok();
    let raw = frames(bytes).unwrap_or_default();
    let (batch, whole) = if valid(bytes) {
        (Some(bytes.to_vec()), true)
    } else {
        let part = legacy_readable(bytes, &raw);
        let part = (!part.is_empty() && part.len() < bytes.len() && valid(&part)).then_some(part);
        (part, false)
    };
    (Sent::read(bytes), whole, batch)
}

struct Compared {
    whole: bool,
    offered: Option<Vec<RecordId>>,
    store: Store,
    memo_heads: usize,
    left: bool,
}

fn compare(initial: &Store, current: &Store, bytes: Vec<u8>) -> Compared {
    compare_received(
        initial,
        current,
        &bytes,
        crate::membership_log::untaken::ReceivedFrames::read(&bytes),
    );
    let mut fragments = Fragments::new(&bytes);
    let received =
        crate::membership_log::untaken::ReceivedFrames::read_from(&mut fragments).unwrap();
    assert_eq!(
        fragments.at,
        bytes.len(),
        "the staged response did not reach EOF"
    );
    compare_received(initial, current, &bytes, received)
}

fn compare_received(
    initial: &Store,
    current: &Store,
    bytes: &[u8],
    received: crate::membership_log::untaken::ReceivedFrames,
) -> Compared {
    let (old_sent, old_whole, old_batch) = legacy_judge(initial, bytes);
    let RecordsJudged {
        stream,
        whole,
        batch,
    } = judge_received_records(initial, "team", received);
    let encoded = batch
        .as_ref()
        .map(|records| join(&records.iter().map(Record::bytes).collect::<Vec<_>>()));
    assert!(
        encoded == old_batch,
        "the selected exact wire bytes changed"
    );
    assert_eq!(whole, old_whole);
    let offered = batch
        .as_ref()
        .map(|records| records.iter().map(|r| *r.id()).collect());
    let mut old_store = current.clone();
    let old_result = old_batch.map(|bytes| old_store.ingest(&bytes).map_err(|e| e.kind()));
    let mut new_store = current.clone();
    let result = batch.map(|records| new_store.ingest_records(records).map_err(|e| e.kind()));
    assert_eq!(result, old_result);
    assert_eq!(new_store.inventory(), old_store.inventory());
    assert_eq!(
        new_store.complete_inventory(),
        old_store.complete_inventory()
    );
    assert!(
        new_store.encode() == old_store.encode(),
        "resulting Store bytes changed"
    );
    assert_eq!(stream.left_in(&new_store), old_sent.left_in(&old_store));

    let floor = initial.frontier();
    let now = Instant::now();
    let old_memo: Untaken<u8> = Untaken::new();
    let new_memo: Untaken<u8> = Untaken::new();
    for (memo, sent, store) in [
        (&old_memo, &old_sent, &old_store),
        (&new_memo, &stream, &new_store),
    ] {
        memo.note(
            0,
            "peer",
            sent,
            &Noted {
                store,
                named_from: &floor,
                now,
                source: None,
            },
        );
    }
    let old_named = old_memo.have(&0, "peer", &old_store, &old_store.have(), now);
    let named = new_memo.have(&0, "peer", &new_store, &new_store.have(), now);
    assert_eq!(named.heads, old_named.heads);
    assert_eq!(named.ids, old_named.ids);
    Compared {
        whole,
        offered,
        left: stream.left_in(&new_store),
        store: new_store,
        memo_heads: named.heads,
    }
}

#[test]
fn reader_failures_are_not_recovered_even_after_layout_failure() {
    let history = History::new();
    let good = history.name(vec![*history.genesis.id()], None, 1);
    let bad_frame = vec![0; good.bytes().len()];
    let bytes = join(&[good.bytes(), &bad_frame, &[0; 32]]);
    for fail_at in [
        5,
        good.bytes().len() / 2,
        good.bytes().len() + bad_frame.len() + 5,
    ] {
        let mut reader = Fragments::new(&bytes);
        reader.fail_at = Some(fail_at);
        let result = crate::membership_log::untaken::ReceivedFrames::read_from(&mut reader);
        assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::UnexpectedEof));
        assert_eq!(reader.at, fail_at);
    }
    let mut reader = Fragments::new(&bytes);
    let received = crate::membership_log::untaken::ReceivedFrames::read_from(&mut reader).unwrap();
    assert_eq!(received.byte_len(), bytes.len());
    assert!(received.whole().is_none());
    assert!(received.readable().is_empty());
}

#[test]
fn decoded_receive_matches_the_byte_oracle_for_unreadable_descendants_and_duplicates() {
    let history = History::new();
    let good = history.name(vec![*history.genesis.id()], None, 1);
    let empty = compare(&history.store, &history.store, Vec::new());
    assert!(empty.whole);
    assert_eq!(empty.offered, Some(Vec::new()));
    let repeated = compare(
        &history.store,
        &history.store,
        join(&[good.bytes(), good.bytes()]),
    );
    assert!(repeated.whole);
    assert_eq!(repeated.offered, Some(vec![*good.id(), *good.id()]));
    assert_eq!(repeated.store.complete_inventory().len(), 2);
    let source = history.name(vec![*good.id()], Some(*good.id()), 2);
    for unknown in [true, false] {
        let mut unread = if unknown {
            crate::membership_log::with_kind_code(&source, 0xee)
        } else {
            source.bytes().to_vec()
        };
        if !unknown {
            *unread.last_mut().unwrap() ^= 1;
        }
        let unread_id = sha256(&unread);
        let child = history.name(vec![unread_id], Some(unread_id), 3);
        let grandchild = history.name(vec![*child.id()], Some(*child.id()), 4);
        for ordered in [true, false] {
            let bytes = if ordered {
                join(&[
                    good.bytes(),
                    &unread,
                    child.bytes(),
                    grandchild.bytes(),
                    good.bytes(),
                    &unread,
                ])
            } else {
                join(&[
                    grandchild.bytes(),
                    child.bytes(),
                    &unread,
                    good.bytes(),
                    &unread,
                    good.bytes(),
                ])
            };
            let actual = compare(&history.store, &history.store, bytes);
            assert!(!actual.whole);
            assert_eq!(actual.offered, Some(vec![*good.id(), *good.id()]));
            assert!(actual.store.complete_bytes(good.id()).is_some());
            assert!(!actual.store.contains(child.id()));
            assert!(!actual.store.contains(grandchild.id()));
            assert!(actual.left);
            assert!(actual.memo_heads > 0);
        }
    }
}

#[test]
fn broken_layout_and_raw_frame_overflow_offer_no_prefix() {
    let history = History::new();
    let good = history.name(vec![*history.genesis.id()], None, 1);
    for mut bytes in [
        join(&[good.bytes(), good.bytes()]),
        join(&[good.bytes(), b"bad"]),
    ] {
        if bytes.ends_with(good.bytes()) {
            bytes.pop();
        }
        let actual = compare(&history.store, &history.store, bytes);
        assert!(!actual.whole);
        assert!(actual.offered.is_none());
        assert_eq!(actual.store.inventory(), history.store.inventory());
        assert_eq!(actual.memo_heads, 0);
    }
    let bytes = good
        .bytes()
        .repeat(crate::membership_log::MAX_HELD_RECORDS + 1);
    let actual = compare(&history.store, &history.store, bytes);
    assert!(actual.offered.is_none());
    assert_eq!(actual.memo_heads, 0);
}

#[test]
fn decoded_receive_preserves_seat_order_and_current_store_floor() {
    let history = History::new();
    let (writer, incarnation, _, entrant) = entrant(7);
    let admit = Draft {
        network: *history.genesis.id(),
        sequence: 1,
        previous: None,
        parents: vec![*history.genesis.id()],
        authored_ms: 2,
        body: Body::Admit {
            entrant,
            evidence: Evidence::Phrase,
        },
    }
    .sign(&history.writer, history.incarnation)
    .unwrap();
    let parent = Draft {
        network: *history.genesis.id(),
        sequence: 1,
        previous: None,
        parents: vec![*admit.id()],
        authored_ms: 3,
        body: Body::Name {
            name: "missing-parent".into(),
        },
    }
    .sign(&writer, incarnation)
    .unwrap();
    let child = Draft {
        network: *history.genesis.id(),
        sequence: 2,
        previous: Some(*parent.id()),
        parents: vec![*parent.id()],
        authored_ms: 4,
        body: Body::Name {
            name: "waiting-child".into(),
        },
    }
    .sign(&writer, incarnation)
    .unwrap();
    let actual = compare(
        &history.store,
        &history.store,
        join(&[child.bytes(), admit.bytes()]),
    );
    assert!(actual.whole);
    assert!(actual.store.complete_bytes(admit.id()).is_some());
    assert!(actual.store.contains(child.id()));
    assert!(actual.store.complete_bytes(child.id()).is_none());
    assert!(actual.left);

    let replaced = Store::for_vault("team").unwrap();
    let name = history.name(vec![*history.genesis.id()], None, 1);
    let actual = compare(&history.store, &replaced, name.bytes().to_vec());
    assert!(actual.whole);
    assert_eq!(
        actual.memo_heads, 0,
        "a replaced floor cannot authorize a memo"
    );
    assert!(actual.left);
}

#[test]
fn decoded_receive_preserves_fresh_network_fallback_and_capacity_refusal() {
    let mut history = History::new();
    let (_, _, _, other) = entrant(8);
    let other_genesis = Record::found(&[9; 32], vec![other], 1).unwrap();
    let actual = compare(
        &history.store,
        &history.store,
        other_genesis.bytes().to_vec(),
    );
    assert!(
        actual.whole,
        "the fresh Store alternative still accepts its own genesis"
    );
    assert_eq!(actual.store.inventory(), history.store.inventory());

    let mut previous = *history.genesis.id();
    for sequence in 1..crate::membership_log::MAX_HELD_RECORDS as u64 {
        let next = history.name(vec![previous], (sequence > 1).then_some(previous), sequence);
        previous = *next.id();
        history.store.insert(next).unwrap();
    }
    assert_eq!(history.store.len(), crate::membership_log::MAX_HELD_RECORDS);
    let beyond = history.name(
        vec![previous],
        Some(previous),
        crate::membership_log::MAX_HELD_RECORDS as u64,
    );
    let actual = compare(&history.store, &history.store, beyond.bytes().to_vec());
    assert!(actual.whole);
    assert!(!actual.store.contains(beyond.id()));
    assert!(actual.left);
}
