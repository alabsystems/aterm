// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{
    database::Change,
    documents::materialized::{Read, materialize},
};
use astream_wire::sync::Header;
use aterm_json::Value;
use aterm_secrets::commons::finality;

const DAY: u64 = 24 * 60 * 60 * 1000;

fn writer(n: u8) -> Actor {
    [n; 48]
}

/// A three-writer history of `records` writes: each names its writer's previous record and,
/// often, another writer's head as it last heard it; record `n` is authored on day `n`.
struct Table {
    headers: Vec<Header>,
    authored: BTreeMap<Digest, u64>,
    changes: BTreeMap<Digest, Vec<Change>>,
}

fn table(records: usize) -> Table {
    let mut tips: BTreeMap<u8, (u64, Digest)> = BTreeMap::new();
    let mut heard: BTreeMap<u8, Digest> = BTreeMap::new();
    let mut table = Table {
        headers: Vec::new(),
        authored: BTreeMap::new(),
        changes: BTreeMap::new(),
    };
    for n in 0..records {
        let by = (n * 7 % 3) as u8 + 1;
        let mut id = [0; 32];
        id[0] = (n * 89 % 251) as u8;
        id[1..9].copy_from_slice(&(n as u64).to_be_bytes());
        let (sequence, previous) = match tips.get(&by) {
            Some(&(sequence, tip)) => (sequence + 1, Some(tip)),
            None => (1, None),
        };
        let mut parents: BTreeSet<Digest> = previous.into_iter().collect();
        if n % 2 == 0
            && let Some(other) = heard.get(&(by % 3 + 1))
        {
            parents.insert(*other);
        }
        table.headers.push(Header {
            id,
            actor: writer(by),
            sequence,
            previous,
            parents: parents.into_iter().collect(),
        });
        tips.insert(by, (sequence, id));
        heard.insert(by, id);
        table.authored.insert(id, n as u64 * DAY);
        table.changes.insert(
            id,
            vec![Change {
                collection: "packages".into(),
                key: format!("TK-{:02}", n % 9),
                value: (n % 11 != 0).then(|| Value::from(n as u64)),
            }],
        );
    }
    table
}

fn index_of<'h>(headers: impl IntoIterator<Item = &'h Header>) -> Index {
    let mut index = Index::new(4096);
    for header in headers {
        index.insert(header.clone()).unwrap();
    }
    index
}

const SCOPE: Digest = [7; 32];

fn body_at(table: &Table, index: &Index, frontier: &BTreeMap<Actor, Tip>) -> Result<Body> {
    let projection = index.project();
    let authored = |id: &Digest| table.authored.get(id).copied();
    let state = |cut: &BTreeSet<Digest>| state_of(table, index, &projection, cut);
    cut_at(
        &History {
            scope: SCOPE,
            generation: 1,
            index,
            projection: &projection,
            authored: &authored,
            state: &state,
        },
        frontier,
    )
}

/// Three writers: the body proposed at a cut every writer has seen is the one a second replica,
/// which received the same records in another order, computes at that frontier, byte for byte,
/// so two writers' shares are two signatures of one body; two of the three recent seats make it
/// final and one does not. A body with a forged state or a forged count of the records below is
/// refused by a replica that recomputes it, and a replica missing a record of the cut cannot
/// compute it at all.
/// NEGATIVE CONTROLS: with `cut_at` leaving the candidates' writers out of the state digest, a
/// forged dot passes: `forged dot accepted`; with the boundary taking in the cut records a held
/// record above the cut names, the replica holding fewer above it signs another body: `one body,
/// two writers`.
#[test]
fn three_writers_finalize_at_two_and_a_forged_state_is_refused() {
    let table = table(90);
    let index = index_of(&table.headers);
    let projection = index.project();
    let authored = |id: &Digest| table.authored.get(id).copied();
    let state = |cut: &BTreeSet<Digest>| state_of(&table, &index, &projection, cut);
    let history = History {
        scope: SCOPE,
        generation: 1,
        index: &index,
        projection: &projection,
        authored: &authored,
        state: &state,
    };
    let writers: BTreeSet<Actor> = (1..=3).map(writer).collect();
    // Records authored at least 14 days before day 89 may be below the cut.
    let body = propose(&history, &writers, 75 * DAY).expect("a cut");
    assert_eq!(body.frontier.len(), 3);
    assert!(body.below > 30 && body.below <= 76, "{}", body.below);
    let floor = boundary(&index, &body.frontier);
    assert_eq!(floor.len(), 3, "one floor record per writer");
    assert_eq!(boundary_digest(&floor), body.boundary);

    // A second replica, which received the records in reverse order and holds fewer above the
    // cut (none after day 80): the body is a function of the cut alone.
    let other = index_of(table.headers.iter().take(81).rev());
    let second = body_at(&table, &other, &body.frontier).unwrap();
    assert_eq!(encode(&second), encode(&body), "one body, two writers");
    assert_eq!(decode(&encode(&body)), Some(body.clone()));
    let mut trailing = encode(&body);
    trailing.push(0);
    assert_eq!(decode(&trailing), None, "one encoding only");

    // Finality: three recent seats, two shares.
    let recent = finality::recent(&writers, 89 * DAY, |_| Some(88 * DAY));
    assert!(finality::is_final(
        &recent,
        &BTreeSet::from([writer(1), writer(2)])
    ));
    assert!(!finality::is_final(&recent, &BTreeSet::from([writer(3)])));

    verify(&history, &body).unwrap();
    let mut forged = body.clone();
    forged.state[0] ^= 1;
    assert!(verify(&history, &forged).is_err(), "forged state accepted");
    let mut forged = body.clone();
    forged.below -= 1;
    assert!(verify(&history, &forged).is_err(), "forged count accepted");
    let mut forged = body.clone();
    forged.generation = 2;
    assert!(
        verify(&history, &forged).is_err(),
        "another generation accepted"
    );

    // The state digest names each candidate's writer: a cell whose winning record were credited
    // to another writer is another state.
    let cells_body = cells_digest(
        &cells_at(&table, &index, &projection, &cut_set(&index, &body)),
        |id| index.get(id).map(|h| (h.actor, h.sequence)),
    )
    .unwrap();
    let swapped = cells_digest(
        &cells_at(&table, &index, &projection, &cut_set(&index, &body)),
        |id| index.get(id).map(|h| (writer(9), h.sequence)),
    )
    .unwrap();
    assert_eq!(cells_body, body.state);
    assert_ne!(swapped, body.state, "forged dot accepted");

    // A replica missing one record of the cut cannot compute it.
    let tip = body.frontier.values().next().unwrap().digest;
    let missing = index.get(&tip).unwrap().previous.unwrap();
    let partial = index_of(table.headers.iter().filter(|h| h.id != missing));
    assert!(body_at(&table, &partial, &body.frontier).is_err());
}

/// The cells the records of `cut` make.
fn cells_at(
    table: &Table,
    index: &Index,
    projection: &Projection,
    cut: &BTreeSet<Digest>,
) -> Cells {
    let ordered: Vec<Digest> = projection
        .ordered
        .iter()
        .filter(|id| cut.contains(*id))
        .copied()
        .collect();
    materialize(
        None,
        Read {
            retained: index.inventory().copied().collect(),
            ordered: &ordered,
            skip: &BTreeSet::new(),
            quarantined: &projection.quarantined,
        },
        |id: &Digest| table.changes.get(id).map(Vec::as_slice),
        &|later, earlier| projection.observes(index, later, earlier),
    )
    .cells
}

/// The document state at `cut`: its cells' digest.
fn state_of(
    table: &Table,
    index: &Index,
    projection: &Projection,
    cut: &BTreeSet<Digest>,
) -> Result<Digest> {
    cells_digest(&cells_at(table, index, projection, cut), |id| {
        index.get(id).map(|h| (h.actor, h.sequence))
    })
}

/// The records of `body`'s cut.
fn cut_set(index: &Index, body: &Body) -> BTreeSet<Digest> {
    let mut cut = BTreeSet::new();
    for tip in body.frontier.values() {
        cut.insert(tip.digest);
        cut.extend(index.ancestors(&tip.digest));
    }
    cut
}

/// No cut while a writer has written nothing, or while every record is too young.
#[test]
fn no_checkpoint_without_every_writer_or_an_old_enough_record() {
    let table = table(30);
    let index = index_of(&table.headers);
    let projection = index.project();
    let authored = |id: &Digest| table.authored.get(id).copied();
    let state = |cut: &BTreeSet<Digest>| state_of(&table, &index, &projection, cut);
    let history = History {
        scope: SCOPE,
        generation: 1,
        index: &index,
        projection: &projection,
        authored: &authored,
        state: &state,
    };
    let with_silent: BTreeSet<Actor> = (1..=4).map(writer).collect();
    assert!(propose(&history, &with_silent, 29 * DAY).is_none());
    let writers: BTreeSet<Actor> = (1..=3).map(writer).collect();
    assert!(
        propose(&history, &writers, 0).is_none() || {
            // Day 0 holds only the first record: a cut of one record at most.
            propose(&history, &writers, 0).is_some_and(|body| body.below <= 1)
        }
    );
}
