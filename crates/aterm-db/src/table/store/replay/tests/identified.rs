// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A replay's records identified apart from its claim ([`identify_onto`], [`witnessed`]) and the
//! writer's replay built on the same steps ([`Builder`]), held to the replay that pushed each
//! record through both at once, kept here as it stood ([`Interleaved`]).

use super::*;
use astream_broker::BrokerRecord;
use astream_wire::sync::Digest;
use std::collections::BTreeSet;

/// The replay as one push per record, the record's identification and its claim checks
/// interleaved, as it stood before they were split: the oracle.
struct Interleaved {
    acknowledged: Witness,
    pending: bool,
    witness: Witness,
    prefix_checked: bool,
    entries: Vec<Entry>,
}
impl Interleaved {
    fn new(acknowledged: Witness, pending: bool) -> Self {
        let cap = usize::try_from(acknowledged.count)
            .unwrap_or(0)
            .min(crate::table::store::JOURNAL_RECORDS);
        Self {
            acknowledged,
            pending,
            witness: Witness::default(),
            prefix_checked: acknowledged == Witness::default(),
            entries: Vec::with_capacity(cap),
        }
    }

    fn push(&mut self, entry: BrokerRecord) -> Result<Offset> {
        if self.pending {
            return Err(Error::new("unfinished table enrollment contains records"));
        }
        if !matches!(entry.subject.as_str(), SUBJECT | CONFLICT_SUBJECT) || entry.body.len() < 8 {
            return Err(Error::new("foreign table journal record"));
        }
        let received_ms = u64::from_le_bytes(entry.body[..8].try_into().unwrap());
        let mut wire = entry.body;
        wire.drain(..8);
        let wire = Identified::new(wire);
        let id = wire.id();
        self.witness = self.witness.advance(entry.seq.0, id, received_ms)?;
        if self.witness.count > self.acknowledged.count + 1 {
            return Err(Error::new(
                "table log exceeds its single unwitnessed append allowance",
            ));
        }
        if self.witness.count == self.acknowledged.count {
            if self.witness != self.acknowledged {
                return Err(Error::new(
                    "acknowledged table history differs from custody witness",
                ));
            }
            self.prefix_checked = true;
        }
        self.entries.push(Entry {
            receipt: Receipt {
                id,
                received_ms,
                offset: entry.seq.0,
            },
            wire,
            conflict: entry.subject == CONFLICT_SUBJECT,
        });
        Ok(Offset(
            entry
                .seq
                .0
                .checked_add(1)
                .ok_or_else(|| Error::new("table offset exhausted"))?,
        ))
    }

    fn finish(self) -> Result<Replay> {
        if !self.prefix_checked {
            return Err(Error::new(
                "table history lost acknowledged custody records",
            ));
        }
        Ok(Replay {
            entries: self.entries,
            witness: self.witness,
        })
    }
}

/// The observer's replay as it stood: every record pushed, then the tail past the claim cut.
fn interleaved_observed(records: Vec<BrokerRecord>, acknowledged: Witness) -> Result<Replay> {
    let mut builder = Interleaved::new(acknowledged, false);
    for record in records {
        builder.push(record)?;
    }
    let mut replay = builder.finish()?;
    let count = usize::try_from(acknowledged.count)
        .map_err(|_| Error::new("table witness count exceeds this reader"))?;
    replay.entries.truncate(count);
    replay.witness = acknowledged;
    Ok(replay)
}

/// What a replay answered: each entry's receipt, wire bytes, identifier and conflict flag, and the
/// replay's witness; or the refusal's text.
type Answer = std::result::Result<(Vec<(Receipt, Vec<u8>, Digest, bool)>, [u8; 40]), String>;

fn answer(replay: Result<Replay>) -> Answer {
    replay
        .map(|replay| {
            let entries = replay
                .entries
                .iter()
                .map(|entry| {
                    (
                        entry.receipt,
                        entry.wire.wire().to_vec(),
                        entry.wire.id(),
                        entry.conflict,
                    )
                })
                .collect();
            (entries, replay.witness.bytes())
        })
        .map_err(|error| error.to_string())
}

/// `n` in-order records of a table journal, operation records and conflict certificates in turn,
/// each body its receipt time and then its wire.
fn journal(n: u64) -> Vec<BrokerRecord> {
    (0..n)
        .map(|seq| {
            let mut body = (1_000 + seq).to_le_bytes().to_vec();
            body.extend(std::iter::repeat_n(u8::try_from(seq).unwrap(), 20));
            BrokerRecord {
                seq: Offset(seq),
                producer_id: 1,
                producer_seq: seq + 1,
                subject: if seq % 2 == 0 {
                    SUBJECT
                } else {
                    CONFLICT_SUBJECT
                }
                .into(),
                body,
                commit: None,
            }
        })
        .collect()
}

/// A writer's replay of `records` ([`Builder`]), or the oracle's, pushed as `read` pushes them.
fn written(records: Vec<BrokerRecord>, acknowledged: Witness, pending: bool) -> Answer {
    let mut builder = Builder::new(acknowledged, pending);
    let replay = records
        .into_iter()
        .try_for_each(|record| builder.push(record).map(drop))
        .and_then(|()| builder.finish());
    answer(replay)
}
fn written_interleaved(records: Vec<BrokerRecord>, acknowledged: Witness, pending: bool) -> Answer {
    let mut builder = Interleaved::new(acknowledged, pending);
    let replay = records
        .into_iter()
        .try_for_each(|record| builder.push(record).map(drop))
        .and_then(|()| builder.finish());
    answer(replay)
}

/// A journal's records identified apart from any claim and then witnessed against one answer
/// every claim as the replay that pushed each record through its identification and its claim
/// checks in turn: a clean journal, and one with a foreign subject, a body too short for its
/// receipt time or a record out of order at each offset, against no claim, the chain through
/// each record, each of those with a bit of its hash flipped, and claims one and two records past
/// the journal, give the same entries (receipt, wire bytes, identifier, conflict flag) and witness,
/// or the same refusal. The writer's replay built on the same steps answers each as the
/// interleaved writer replay does, its enrollment pending and not. CONTROL: every refusal either
/// replay has is seen, and the claim checks' refusals come before a later record's.
/// NEGATIVE CONTROLS: `witnessed` answering the identification's refusal before it admits the
/// links answers "foreign table journal record" where the oracle says "acknowledged table history
/// differs from custody witness"; `Claim::admit` taking a claim as checked without comparing it
/// admits the flipped chain, so a later record is refused for the allowance (or none is) where
/// the oracle refuses the claim; the writer's push identifying before it checks for a pending
/// enrollment answers a pending enrollment's foreign first record "foreign table journal record",
/// where the oracle says "unfinished table enrollment contains records".
#[test]
fn identified_records_witness_every_claim_as_the_interleaved_replay() {
    const N: u64 = 5;
    let clean = journal(N);
    let mut chain = vec![Witness::default()];
    for record in &clean {
        let received_ms = u64::from_le_bytes(record.body[..8].try_into().unwrap());
        let id = aterm_digest::Sha256::digest(&record.body[8..]);
        let next = chain
            .last()
            .unwrap()
            .advance(record.seq.0, id, received_ms)
            .unwrap();
        chain.push(next);
    }
    let with = |witness: Witness, edit: fn(&mut [u8; 40])| {
        let mut bytes = witness.bytes();
        edit(&mut bytes);
        Witness::decode(&bytes).unwrap()
    };
    let mut claims: Vec<(String, Witness)> = chain
        .iter()
        .enumerate()
        .map(|(k, witness)| (format!("the chain through {k}"), *witness))
        .collect();
    for (k, witness) in chain.iter().enumerate().skip(1) {
        claims.push((
            format!("the chain through {k}, a hash bit flipped"),
            with(*witness, |bytes| bytes[8] ^= 1),
        ));
    }
    let last = *chain.last().unwrap();
    claims.push((
        "one past the journal".into(),
        with(last, |bytes| {
            bytes[..8].copy_from_slice(&(N + 1).to_le_bytes())
        }),
    ));
    claims.push((
        "two past the journal".into(),
        with(last, |bytes| {
            bytes[..8].copy_from_slice(&(N + 2).to_le_bytes())
        }),
    ));

    let mut journals = vec![("clean".to_owned(), clean.clone())];
    for at in 0..usize::try_from(N).unwrap() {
        let mut foreign = clean.clone();
        foreign[at].subject = "commons/other/1".into();
        journals.push((format!("a foreign subject at {at}"), foreign));
        let mut short = clean.clone();
        short[at].body.truncate(7);
        journals.push((format!("a short body at {at}"), short));
        let mut unordered = clean.clone();
        unordered[at].seq = Offset(unordered[at].seq.0 + 1);
        journals.push((format!("a record out of order at {at}"), unordered));
    }

    let mut refusals = BTreeSet::new();
    let mut claim_before_refusal = false;
    for (what, records) in &journals {
        for (claim, acknowledged) in &claims {
            let oracle = answer(interleaved_observed(records.clone(), *acknowledged));
            assert_eq!(
                answer(read_observed(records.clone(), *acknowledged)),
                oracle,
                "{what}, against {claim}: an observer"
            );
            let mut links = Vec::new();
            if let Err(identification) = identify_onto(&mut links, records.clone())
                && oracle
                    .as_ref()
                    .is_err_and(|refusal| *refusal != identification.to_string())
            {
                claim_before_refusal = true;
            }
            refusals.extend(oracle.err());
            for pending in [false, true] {
                let oracle = written_interleaved(records.clone(), *acknowledged, pending);
                assert_eq!(
                    written(records.clone(), *acknowledged, pending),
                    oracle,
                    "{what}, against {claim}, pending {pending}: a writer"
                );
                refusals.extend(oracle.err());
            }
        }
    }
    assert_eq!(
        refusals,
        BTreeSet::from(
            [
                "acknowledged table history differs from custody witness",
                "foreign table journal record",
                "table custody witness is not contiguous",
                "table history lost acknowledged custody records",
                "table log exceeds its single unwitnessed append allowance",
                "unfinished table enrollment contains records",
            ]
            .map(String::from)
        ),
        "CONTROL: every refusal a replay has is seen"
    );
    assert!(
        claim_before_refusal,
        "CONTROL: a claim's refusal comes before a later record's"
    );
}
