// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use astream_wire::sync::{Header, Index, Projection};
use aterm_json::Value;
use std::cell::Cell as Counter;
use std::collections::BTreeMap;

/// A deterministic xorshift, so a failing seed reruns as it failed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn actor(n: u8) -> Actor {
    [n; 48]
}

/// A generated history: headers in authored order, each operation's changes.
struct History {
    headers: Vec<Header>,
    changes: BTreeMap<Digest, Vec<Change>>,
}

/// `records` operations by `writers` writers over `rows` rows. Each names its writer's previous
/// operation and up to `seen` other writers' latest ones. Some write an absence (a revert's
/// compensating value), and with `forks`, a writer now and then signs a second operation at a
/// sequence it already used, which quarantines it.
fn history(rng: &mut Rng, records: u64, writers: u8, rows: u64, seen: u64, forks: bool) -> History {
    let mut heads: BTreeMap<u8, (u64, Digest)> = BTreeMap::new();
    let mut headers = Vec::new();
    let mut changes = BTreeMap::new();
    for n in 0..records {
        let writer = u8::try_from(rng.below(u64::from(writers))).unwrap() + 1;
        let mut id = [0; 32];
        id[..8].copy_from_slice(&(n + 1).to_be_bytes());
        id[8] = writer;
        let fork = forks && rng.below(40) == 0 && heads.get(&writer).is_some_and(|h| h.0 > 1);
        let (sequence, previous) = match heads.get(&writer) {
            // A second operation at the writer's latest sequence, on the same predecessor.
            Some(&(sequence, _)) if fork => {
                let prior = headers
                    .iter()
                    .rev()
                    .find(|h: &&Header| h.actor == actor(writer) && h.sequence == sequence)
                    .and_then(|h| h.previous);
                (sequence, prior)
            }
            Some(&(sequence, head)) => (sequence + 1, Some(head)),
            None => (1, None),
        };
        let mut parents: BTreeSet<Digest> = previous.into_iter().collect();
        for _ in 0..rng.below(seen + 1) {
            let other = u8::try_from(rng.below(u64::from(writers))).unwrap() + 1;
            if other != writer
                && let Some((_, head)) = heads.get(&other)
            {
                parents.insert(*head);
            }
        }
        headers.push(Header {
            id,
            actor: actor(writer),
            sequence,
            previous,
            parents: parents.into_iter().collect(),
        });
        if !fork {
            heads.insert(writer, (sequence, id));
        }
        let row = rng.below(rows);
        let value = (rng.below(8) != 0).then(|| Value::from(n));
        changes.insert(
            id,
            vec![Change {
                collection: "rows".into(),
                key: row.to_string(),
                value,
            }],
        );
    }
    History { headers, changes }
}

fn index_of(headers: &[Header]) -> Index {
    let mut index = Index::new(headers.len().max(1) * 2);
    for header in headers {
        index.insert(header.clone()).unwrap();
    }
    index
}

/// Random histories with forks and reverts arrive out of order in batches, while some
/// operations are held back from the cells (an opaque author's, until its payload arrives) and
/// the skipped set changes now and then. After every batch the cells extended from the previous
/// read equal the cells rebuilt from nothing, which equal the cells applied with the old
/// graph walk (`Index::observes`) instead of the projection's version vectors.
/// NEGATIVE CONTROLS: with `materialize` reusing cells without the `!previous.retained` clause,
/// a held-back operation released under an applied descendant stays a candidate beside it
/// (`seed 2, 48 arrived`, two candidates on the left); without the subset clause, a fork's
/// withdrawn operations stay in the cells (`seed 7, 26 arrived`).
#[test]
fn incremental_view_equals_full_rebuild() {
    let (mut extended, mut rebuilt, mut released) = (0, 0, 0);
    for seed in 1..=60_u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let History { headers, changes } = history(&mut rng, 160, 5, 12, 2, true);
        let mut arrival: Vec<Header> = headers.clone();
        for at in (1..arrival.len()).rev() {
            // A local shuffle: arrivals run ahead of their parents, but not by much.
            let with = at.saturating_sub(usize::try_from(rng.below(6)).unwrap());
            arrival.swap(at, with);
        }
        let mut previous: Option<Materialized> = None;
        let mut skip = BTreeSet::new();
        let mut opaque: BTreeSet<Digest> = BTreeSet::new();
        let mut arrived = 0;
        while arrived < arrival.len() {
            arrived = (arrived + 1 + usize::try_from(rng.below(24)).unwrap()).min(arrival.len());
            let index = index_of(&arrival[..arrived]);
            let projection = index.project();
            // An operation of the batch is now and then held back; one held back is released
            // later, under whatever applied operations named it meanwhile.
            for header in &arrival[arrived.saturating_sub(24)..arrived] {
                if rng.below(15) == 0 {
                    opaque.insert(header.id);
                }
            }
            if rng.below(3) == 0
                && let Some(&release) = opaque.iter().next()
            {
                opaque.remove(&release);
                released += 1;
            }
            if rng.below(10) == 0 {
                skip = projection
                    .ordered
                    .iter()
                    .filter(|_| rng.below(12) == 0)
                    .copied()
                    .collect();
            }
            let ordered: Vec<Digest> = projection
                .ordered
                .iter()
                .filter(|id| !opaque.contains(*id))
                .copied()
                .collect();
            let read = || Read {
                retained: index.inventory().copied().collect(),
                ordered: &ordered,
                skip: &skip,
                quarantined: &projection.quarantined,
            };
            let changes_of = |id: &Digest| changes.get(id).map(Vec::as_slice);
            let vectors =
                |later: &Digest, earlier: &Digest| projection.observes(&index, later, earlier);
            let walk = |later: &Digest, earlier: &Digest| index.observes(later, earlier);
            let incremental = materialize(previous.take(), read(), changes_of, &vectors);
            let full = materialize(None, read(), changes_of, &vectors);
            let walked = materialize(None, read(), changes_of, &walk);
            assert_eq!(
                incremental.cells, full.cells,
                "seed {seed}, {arrived} arrived"
            );
            assert_eq!(full.cells, walked.cells, "seed {seed}, {arrived} arrived");
            if incremental.extended {
                extended += 1;
            } else {
                rebuilt += 1;
            }
            previous = Some(incremental);
        }
    }
    // Both paths ran, and a held-back operation was released under its descendants.
    assert!(
        extended > 100 && rebuilt > 20 && released > 50,
        "{extended} {rebuilt} {released}"
    );
}

/// A 20,000-operation table is applied with at most 20 graph questions per operation, each
/// answered by the projection's version vectors, which cover every operation applied, so the
/// read walks no ancestor chain: its cost grows with the operations, not with their square.
/// NEGATIVE CONTROL: with the projection's vectors left empty (`Projection::default()`'s, as a
/// projection built outside astream has), the coverage assertion fails at `records: 0`.
#[test]
fn view_at_20000_records_is_subquadratic() {
    const RECORDS: u64 = 20_000;
    let mut rng = Rng(0x005E_ED0B_5E57_A7E5);
    let History { headers, changes } = history(&mut rng, RECORDS, 8, 400, 2, false);
    let index = index_of(&headers);
    let started = std::time::Instant::now();
    let projection: Projection = index.project();
    assert_eq!(projection.ordered.len(), headers.len());
    // Every applied operation has a vector row, so `Projection::observes` answers each
    // question about it by a lookup, never a walk.
    let covered = format!("{:?}", projection.vectors);
    assert!(
        covered.contains(&format!("records: {RECORDS}")),
        "{covered}"
    );
    let asked = Counter::new(0_u64);
    let observes = |later: &Digest, earlier: &Digest| {
        asked.set(asked.get() + 1);
        projection.observes(&index, later, earlier)
    };
    let made = materialize(
        None,
        Read {
            retained: index.inventory().copied().collect(),
            ordered: &projection.ordered,
            skip: &BTreeSet::new(),
            quarantined: &projection.quarantined,
        },
        |id| changes.get(id).map(Vec::as_slice),
        &observes,
    );
    let elapsed = started.elapsed();
    eprintln!(
        "{RECORDS} records: {} graph questions, {} cells, {elapsed:?}",
        asked.get(),
        made.cells.len()
    );
    assert!(asked.get() <= 20 * RECORDS, "{} questions", asked.get());
    assert!(!made.cells.is_empty());
}
