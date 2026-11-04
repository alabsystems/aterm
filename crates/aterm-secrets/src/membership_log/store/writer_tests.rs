// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::age::Identity;
use crate::commons::binding::Request;
use crate::membership_log::Entrant;
use std::io::{self, Write};

fn histories() -> (Store, Vec<Record>, Store, Vec<Record>) {
    let seed = [41; 32];
    let incarnation = [42; 16];
    let identity = Identity::from_secret_bytes([43; 32]);
    let request =
        Request::create([44; 16], [45; 32], [46; 16], incarnation, &identity, &seed).unwrap();
    let actor = request.binding().actor();
    let genesis = Record::found(
        &[47; 32],
        vec![Entrant {
            request,
            node_key: [48; 32],
            name: "writer".into(),
            label: "m1".into(),
        }],
        1,
    )
    .unwrap();
    let mut log = Log::new(genesis.clone()).unwrap();
    let sign = |log: &Log, name: &str| {
        log.draft(&actor, 2, Body::Name { name: name.into() })
            .sign(&seed, incarnation)
            .unwrap()
    };
    // Two genuine concurrent siblings. The canonical complete prefix is their held order,
    // not a new sort of the full stream during serialization.
    let left = sign(&log, "left");
    let right = sign(&log, "right");
    let (left, right) = if left.id() < right.id() {
        (left, right)
    } else {
        (right, left)
    };
    assert!(right.id() > left.id());
    log.insert(right.clone()).unwrap();
    log.insert(left.clone()).unwrap();
    let missing = sign(&log, "missing");
    log.insert(missing.clone()).unwrap();
    let child = sign(&log, "child");
    log.insert(child.clone()).unwrap();
    let grandchild = sign(&log, "grandchild");

    let mut rooted = Store::for_vault("writer").unwrap();
    for record in [&genesis, &right, &left, &grandchild, &child] {
        rooted.insert(record.clone()).unwrap();
    }
    assert_eq!(rooted.complete_inventory().len(), 3);
    assert_eq!(rooted.len(), 5);
    let complete_and_waiting = vec![genesis, right, left, child.clone(), grandchild.clone()];
    let mut unrooted = Store::for_vault("writer").unwrap();
    for record in [&grandchild, &child, &missing] {
        unrooted.insert(record.clone()).unwrap();
    }
    assert!(unrooted.log().is_none());
    let pending = vec![missing, child, grandchild];
    (rooted, complete_and_waiting, unrooted, pending)
}

fn expected(records: &[Record]) -> Vec<u8> {
    let mut out = Vec::new();
    for record in records {
        out.extend_from_slice(record.bytes());
    }
    out
}

#[test]
fn writer_keeps_the_exact_complete_prefix_and_causal_pending_suffix() {
    let (rooted, records, unrooted, pending) = histories();
    for (store, records) in [(rooted, records), (unrooted, pending)] {
        let expected = expected(&records);
        let mut written = Vec::new();
        store.write_encoded(&mut written).unwrap();
        assert_eq!(written, expected);
        assert_eq!(store.encode(), expected);
        assert_eq!(
            decode_records(&written)
                .unwrap()
                .iter()
                .map(|r| *r.id())
                .collect::<Vec<_>>(),
            records.iter().map(|r| *r.id()).collect::<Vec<_>>()
        );
    }
}

struct ShortWriter {
    bytes: Vec<u8>,
    interrupt: bool,
    limit: usize,
    fail_at: Option<usize>,
}

impl Write for ShortWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if std::mem::take(&mut self.interrupt) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        if self.fail_at == Some(self.bytes.len()) {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let mut n = self.limit.min(bytes.len());
        if let Some(end) = self.fail_at {
            n = n.min(end - self.bytes.len());
        }
        self.bytes.extend_from_slice(&bytes[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn writer_retries_interruptions_and_short_writes_but_preserves_errors() {
    let (rooted, records, _, _) = histories();
    let expected = expected(&records);
    let mut writer = ShortWriter {
        bytes: Vec::new(),
        interrupt: true,
        limit: 3,
        fail_at: None,
    };
    rooted.write_encoded(&mut writer).unwrap();
    assert_eq!(writer.bytes, expected);
    writer.bytes.clear();
    writer.fail_at = Some(8);
    let error = rooted.write_encoded(&mut writer).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    assert_eq!(writer.bytes, expected[..8]);
    writer.bytes.clear();
    writer.fail_at = None;
    writer.limit = 0;
    assert_eq!(
        rooted.write_encoded(&mut writer).unwrap_err().kind(),
        io::ErrorKind::WriteZero
    );
    assert!(writer.bytes.is_empty());
}

#[test]
fn held_suffix_writer_and_length_exclude_waiting_records_at_every_offset() {
    let (rooted, records, unrooted, _) = histories();
    for from in [0, 1, 2, 3, 4, usize::MAX] {
        let expected = expected(records.get(from..3).unwrap_or_default());
        let mut written = Vec::new();
        rooted.write_held_from(from, &mut written).unwrap();
        assert_eq!(written, expected);
        assert_eq!(rooted.held_len_from(from).unwrap(), expected.len());
        assert_eq!(rooted.encode_held_from(from), expected);
        let mut absent = Vec::new();
        unrooted.write_held_from(from, &mut absent).unwrap();
        assert!(absent.is_empty());
        assert_eq!(unrooted.held_len_from(from).unwrap(), 0);
    }
}
