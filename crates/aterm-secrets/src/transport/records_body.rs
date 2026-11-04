// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A bounded wire reader over one immutable record selection.

use crate::membership_log::{MAX_RECORDS_STREAM, Record};
use crate::{Error, Result};
use std::io::{self, Read};

pub(super) struct RecordsReader<'a> {
    records: &'a [Record],
    len: usize,
    next: usize,
    offset: usize,
}

impl<'a> RecordsReader<'a> {
    pub(super) fn new(records: &'a [Record]) -> Result<Self> {
        let len = records.iter().try_fold(0usize, |len, record| {
            len.checked_add(record.bytes().len())
                .filter(|len| *len <= MAX_RECORDS_STREAM)
        });
        let len = len.ok_or_else(|| {
            Error::new(format!(
                "the record batch is larger than {MAX_RECORDS_STREAM} bytes"
            ))
        })?;
        Ok(Self {
            records,
            len,
            next: 0,
            offset: 0,
        })
    }

    pub(super) const fn len(&self) -> usize {
        self.len
    }
}

impl Read for RecordsReader<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let mut copied = 0;
        while copied < output.len() {
            let Some(record) = self.records.get(self.next) else {
                break;
            };
            let bytes = &record.bytes()[self.offset..];
            let take = bytes.len().min(output.len() - copied);
            output[copied..copied + take].copy_from_slice(&bytes[..take]);
            copied += take;
            self.offset += take;
            if self.offset == record.bytes().len() {
                self.next += 1;
                self.offset = 0;
            }
        }
        Ok(copied)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::age::Identity;
    use crate::commons::binding::Request;
    use crate::membership_log::{Body, Draft, Entrant, EpochAudience, Log, Store};

    fn history() -> (Store, Vec<Record>, Record) {
        let writer = [31; 32];
        let incarnation = [32; 16];
        let identity = Identity::from_secret_bytes([33; 32]);
        let request = Request::create(
            [34; 16],
            [35; 32],
            [36; 16],
            incarnation,
            &identity,
            &writer,
        )
        .unwrap();
        let actor = request.binding().actor();
        let genesis = Record::found(
            &[37; 32],
            vec![Entrant {
                request,
                node_key: [38; 32],
                name: "reader".into(),
                label: "m1".into(),
            }],
            1,
        )
        .unwrap();
        let mut log = Log::new(genesis.clone()).unwrap();
        let left = log
            .draft(
                &actor,
                2,
                Body::Name {
                    name: "left".into(),
                },
            )
            .sign(&writer, incarnation)
            .unwrap();
        let right = log
            .draft(
                &actor,
                2,
                Body::Name {
                    name: "right".into(),
                },
            )
            .sign(&writer, incarnation)
            .unwrap();
        let (first, second) = if left.id() > right.id() {
            (left, right)
        } else {
            (right, left)
        };
        assert!(first.id() > second.id());
        log.insert(first.clone()).unwrap();
        log.insert(second.clone()).unwrap();
        let later = log
            .draft(
                &actor,
                3,
                Body::Name {
                    name: "later".into(),
                },
            )
            .sign(&writer, incarnation)
            .unwrap();
        let mut pending = log.clone();
        let parent = pending
            .draft(
                &actor,
                4,
                Body::Name {
                    name: "missing".into(),
                },
            )
            .sign(&writer, incarnation)
            .unwrap();
        let parent_id = *parent.id();
        pending.insert(parent).unwrap();
        let waiting = pending
            .draft(
                &actor,
                5,
                Body::Name {
                    name: "waiting".into(),
                },
            )
            .sign(&writer, incarnation)
            .unwrap();
        let mut store = Store::for_vault("reader").unwrap();
        for record in [&genesis, &first, &second, &waiting] {
            store.insert(record.clone()).unwrap();
        }
        assert_eq!(store.len(), 4);
        assert_eq!(store.complete_inventory().len(), 3);
        assert!(!store.contains(&parent_id));
        (store, vec![genesis, first, second], later)
    }

    fn wire(records: &[Record]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for record in records {
            bytes.extend_from_slice(record.bytes());
        }
        bytes
    }

    #[test]
    fn selected_reader_keeps_complete_order_across_every_output_boundary() {
        let (store, expected, _) = history();
        let selected = store.missing(&[]);
        let expected = wire(&expected);
        for width in [1, 2, 17, selected[0].bytes().len(), 64 * 1024] {
            let mut reader = RecordsReader::new(&selected).unwrap();
            assert_eq!(reader.len(), expected.len());
            assert_eq!(reader.read(&mut []).unwrap(), 0);
            let mut output = Vec::new();
            let mut chunk = vec![0; width];
            loop {
                let read = reader.read(&mut chunk).unwrap();
                if read == 0 {
                    break;
                }
                output.extend_from_slice(&chunk[..read]);
            }
            assert_eq!(output, expected, "output width {width}");
            assert_eq!(reader.read(&mut chunk).unwrap(), 0);
        }
        let mut empty = RecordsReader::new(&[]).unwrap();
        assert_eq!(empty.len(), 0);
        assert_eq!(empty.read(&mut [0; 4]).unwrap(), 0);
    }

    #[test]
    fn selected_reader_retains_its_snapshot_after_the_store_changes_and_drops() {
        let (mut store, expected, later) = history();
        let selected = store.missing(&[]);
        let expected = wire(&expected);
        let mut reader = RecordsReader::new(&selected).unwrap();
        let mut output = vec![0; 5];
        reader.read_exact(&mut output).unwrap();
        store.insert(later).unwrap();
        assert_eq!(store.complete_inventory().len(), 4);
        drop(store);
        reader.read_to_end(&mut output).unwrap();
        assert_eq!(output, expected);
    }

    #[test]
    fn selected_reader_refuses_a_total_past_the_wire_bound() {
        let (store, _, _) = history();
        let log = store.log().unwrap();
        let draft = Draft {
            network: *log.genesis().id(),
            sequence: 1,
            previous: None,
            parents: vec![*log.genesis().id()],
            authored_ms: 5,
            body: Body::Put {
                audience: EpochAudience::Everyone,
                epoch: [42; 32],
                salt: [43; 32],
                ciphertext: vec![44; 512 * 1024].into(),
            },
        };
        let record = draft.sign(&[45; 32], [46; 16]).unwrap();
        let count = MAX_RECORDS_STREAM / record.bytes().len();
        let mut selected = vec![record.clone(); count];
        assert!(RecordsReader::new(&selected).is_ok());
        selected.push(record);
        assert!(RecordsReader::new(&selected).is_err());
    }
}
