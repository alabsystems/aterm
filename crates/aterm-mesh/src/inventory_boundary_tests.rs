// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::stores::{Held, TableKey};
use std::io::{BufReader, BufWriter};
use std::os::unix::net::UnixStream;

const TABLE: Digest = [7; 32];

struct Buffered {
    reader: BufReader<UnixStream>,
    writer: BufWriter<UnixStream>,
}

impl Buffered {
    fn new(stream: UnixStream) -> Self {
        Self {
            reader: BufReader::new(stream.try_clone().unwrap()),
            writer: BufWriter::new(stream),
        }
    }
}

impl Read for Buffered {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.reader.read(bytes)
    }
}

impl Write for Buffered {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.writer.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

/// One unit's physical records remain held while an incoming update can withdraw
/// permission to expose them. The wire exchange must ask again in its send turn.
struct Changing {
    unit: Unit,
    outgoing: Vec<u8>,
    may_send: AtomicBool,
    withdraw_on_receive: bool,
    received: Mutex<Vec<Vec<u8>>>,
}

impl Changing {
    fn new(unit: Unit, wire: &[u8], withdraw_on_receive: bool) -> Arc<Self> {
        Arc::new(Self {
            unit,
            outgoing: wire.to_vec(),
            may_send: AtomicBool::new(true),
            withdraw_on_receive,
            received: Mutex::new(Vec::new()),
        })
    }

    fn records(&self) -> Vec<Vec<u8>> {
        std::iter::once(self.outgoing.clone())
            .chain(self.received.lock().unwrap().iter().cloned())
            .collect()
    }

    fn retain(&self, wires: &[Vec<u8>]) -> Vec<Verdict> {
        self.received.lock().unwrap().extend_from_slice(wires);
        if self.withdraw_on_receive {
            self.may_send.store(false, Ordering::Release);
        }
        vec![Verdict::Applied; wires.len()]
    }

    fn wires(&self, wanted: impl Fn(&[u8]) -> bool) -> Vec<Vec<u8>> {
        if !self.may_send.load(Ordering::Acquire) {
            return Vec::new();
        }
        self.records()
            .into_iter()
            .filter(|wire| wanted(wire))
            .collect()
    }
}

fn proof_key(wire: &[u8]) -> ProofKey {
    let mut key = [0; 81];
    key[49..].copy_from_slice(&crate::stores::digest(wire));
    key
}

impl Stores for Changing {
    fn ingest_table(&self, table: Digest, kind: Kind, wires: &[Vec<u8>]) -> Vec<Verdict> {
        assert_eq!(table, TABLE);
        assert_eq!(
            kind,
            if matches!(self.unit, Unit::Proofs(_)) {
                Kind::Conflict
            } else {
                Kind::Operation
            }
        );
        self.retain(wires)
    }

    fn ingest_members(&self, wires: &[Vec<u8>]) -> Vec<Verdict> {
        assert_eq!(self.unit, Unit::Members);
        self.retain(wires)
    }

    fn table_lengths(&self) -> io::Result<Vec<(TableKey, u64)>> {
        Ok(if self.unit == Unit::Members {
            Vec::new()
        } else {
            vec![(
                TableKey {
                    table: TABLE,
                    recovery: false,
                },
                self.records().len() as u64,
            )]
        })
    }

    fn table_held(&self, key: TableKey, from: u64, upto: u64) -> io::Result<Vec<Held>> {
        if key.table != TABLE || key.recovery || self.unit == Unit::Members {
            return Ok(Vec::new());
        }
        Ok(self
            .records()
            .into_iter()
            .enumerate()
            .filter_map(|(offset, wire)| {
                let offset = offset as u64;
                (from <= offset && offset < upto).then_some(Held {
                    offset,
                    kind: if matches!(self.unit, Unit::Proofs(_)) {
                        Kind::Conflict
                    } else {
                        Kind::Operation
                    },
                    wire,
                })
            })
            .collect())
    }

    fn members_held(&self, from: usize) -> io::Result<(Vec<Vec<u8>>, usize)> {
        let records = if self.unit == Unit::Members {
            self.records()
        } else {
            Vec::new()
        };
        let count = records.len();
        Ok((records.into_iter().skip(from).collect(), count))
    }

    fn tables(&self) -> io::Result<BTreeSet<Digest>> {
        Ok(if self.unit == Unit::Members {
            BTreeSet::new()
        } else {
            BTreeSet::from([TABLE])
        })
    }

    fn takes(&self, table: Digest) -> io::Result<bool> {
        Ok(table == TABLE && self.unit != Unit::Members)
    }

    fn table_inventory(&self, table: Digest) -> io::Result<Option<BTreeSet<Digest>>> {
        Ok((table == TABLE).then(|| {
            if matches!(self.unit, Unit::Table(_)) {
                self.records()
                    .iter()
                    .map(|wire| crate::stores::digest(wire))
                    .collect()
            } else {
                BTreeSet::new()
            }
        }))
    }

    fn table_wires(&self, table: Digest, ids: &[Digest]) -> io::Result<Vec<Vec<u8>>> {
        assert_eq!(table, TABLE);
        Ok(self.wires(|wire| ids.contains(&crate::stores::digest(wire))))
    }

    fn table_proofs(&self, table: Digest) -> io::Result<Option<BTreeSet<ProofKey>>> {
        Ok((table == TABLE).then(|| {
            if matches!(self.unit, Unit::Proofs(_)) {
                self.records().iter().map(|wire| proof_key(wire)).collect()
            } else {
                BTreeSet::new()
            }
        }))
    }

    fn table_proof_wires(&self, table: Digest, keys: &[ProofKey]) -> io::Result<Vec<Vec<u8>>> {
        assert_eq!(table, TABLE);
        Ok(self.wires(|wire| keys.contains(&proof_key(wire))))
    }

    fn members_inventory(&self) -> io::Result<BTreeSet<Digest>> {
        Ok(if self.unit == Unit::Members {
            self.records()
                .iter()
                .map(|wire| crate::stores::digest(wire))
                .collect()
        } else {
            BTreeSet::new()
        })
    }

    fn members_wires(&self, ids: &[Digest]) -> io::Result<Vec<Vec<u8>>> {
        Ok(self.wires(|wire| ids.contains(&crate::stores::digest(wire))))
    }
}

#[test]
fn every_inventory_unit_fetches_current_wires_after_the_receive_turn() {
    for unit in [Unit::Members, Unit::Proofs(TABLE), Unit::Table(TABLE)] {
        for withdraw in [false, true] {
            let a = Changing::new(unit, b"incoming update", false);
            let b = Changing::new(unit, b"withdrawn outgoing record", withdraw);
            let root = aterm_tempfile::tempdir().unwrap();
            let left_dir = root.path().join("left");
            let right_dir = root.path().join("right");
            std::fs::create_dir_all(&left_dir).unwrap();
            std::fs::create_dir_all(&right_dir).unwrap();
            let left_egress = Egress::new(Arc::clone(&a) as Arc<dyn Stores>, &left_dir);
            let right_egress = Egress::new(Arc::clone(&b) as Arc<dyn Stores>, &right_dir);
            let left_summary = summary(a.as_ref()).unwrap();
            let right_summary = summary(b.as_ref()).unwrap();
            let (left, right) = UnixStream::pair().unwrap();
            for stream in [&left, &right] {
                stream
                    .set_read_timeout(Some(Duration::from_secs(60)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(60)))
                    .unwrap();
            }
            let (mut left, mut right) = (Buffered::new(left), Buffered::new(right));
            let (sent, answered) = std::thread::scope(|scope| {
                let responder = scope.spawn(|| {
                    exchange_stream(
                        b.as_ref(),
                        &right_egress,
                        &mut right,
                        right_summary,
                        false,
                        false,
                    )
                    .unwrap()
                });
                let sent = exchange_stream(
                    a.as_ref(),
                    &left_egress,
                    &mut left,
                    left_summary,
                    true,
                    false,
                )
                .unwrap();
                (sent, responder.join().unwrap())
            });
            let outgoing = usize::from(!withdraw);
            assert_eq!((sent.sent, sent.received), (1, outgoing), "{unit:?}");
            assert_eq!(
                (answered.sent, answered.received),
                (outgoing, 1),
                "{unit:?}"
            );
            assert_eq!(
                b.received.lock().unwrap().as_slice(),
                std::slice::from_ref(&a.outgoing)
            );
            assert_eq!(
                *a.received.lock().unwrap(),
                if withdraw {
                    Vec::new()
                } else {
                    vec![b.outgoing.clone()]
                }
            );
            assert!(
                b.records().contains(&b.outgoing),
                "only eligibility changed, not custody"
            );
        }
    }
}
