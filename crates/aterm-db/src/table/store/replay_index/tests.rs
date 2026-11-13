// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use astream_wire::sync::Index;

fn header(id: u8, actor: u8, sequence: u64, previous: Option<u8>) -> Header {
    Header {
        id: [id; 32],
        actor: [actor; 48],
        sequence,
        previous: previous.map(|p| [p; 32]),
        parents: previous.map(|p| vec![[p; 32]]).unwrap_or_default(),
    }
}

/// Two ordinary headers, two different headers under one id, and one whose shape the index refuses.
fn pool() -> [Header; 5] {
    [
        header(1, 1, 1, None),
        header(2, 2, 1, None),
        header(3, 1, 2, Some(1)),
        header(3, 3, 1, None),
        header(4, 2, 0, None),
    ]
}

#[derive(Clone, Copy)]
enum Kind {
    Operation,
    Source,
}

/// An open's first refusal: its message, and the index's refusal where the index refused.
type Refused = (&'static str, Option<Refusal>);

/// The replay loop of `Store::open_region` as it was with an index built as it replayed: each
/// source and each operation inserted into the index, then an operation held unless its id is.
fn indexed(
    index: &mut Index,
    held: &mut BTreeMap<Digest, Header>,
    events: &[(Kind, &Header)],
) -> Result<(), Refused> {
    for (kind, header) in events {
        match kind {
            Kind::Source => {
                index
                    .insert((*header).clone())
                    .map_err(|refusal| ("invalid discarded-source header", Some(refusal)))?;
            }
            Kind::Operation => {
                index
                    .insert((*header).clone())
                    .map_err(|refusal| ("invalid table replay header", Some(refusal)))?;
                if held.contains_key(&header.id) {
                    return Err(("duplicate physical table record", None));
                }
                held.insert(header.id, (*header).clone());
            }
        }
    }
    Ok(())
}

/// The same loop through [`ReplayIndex`], as `Store::open_region` drives it: `kept` gathers the
/// sources the store's certificates keep, `held` its operations.
fn admitted(
    causal: &mut ReplayIndex,
    held: &mut BTreeMap<Digest, Header>,
    kept: &mut Vec<Header>,
    events: &[(Kind, &Header)],
) -> Result<(), Refused> {
    for (kind, header) in events {
        match kind {
            Kind::Source => {
                causal
                    .source(held, |held| held, header)
                    .map_err(|refusal| ("invalid discarded-source header", Some(refusal)))?;
                kept.push((*header).clone());
            }
            Kind::Operation => {
                let Some(slot) = causal
                    .operation(held.entry(header.id), |held| held, header)
                    .map_err(|refusal| ("invalid table replay header", Some(refusal)))?
                else {
                    return Err(("duplicate physical table record", None));
                };
                slot.insert((*header).clone());
            }
        }
    }
    Ok(())
}

fn content(index: &Index) -> Vec<(Digest, Header)> {
    index
        .inventory()
        .map(|id| (*id, index.get(id).unwrap().clone()))
        .collect()
}

/// R1: over every sequence of up to four admissions (an operation or a source, of each of the
/// five headers) at capacities 1 to 3, the loop through [`ReplayIndex`] gives the same first error
/// (or none), the same held operations, and headers whose index (`Store::index`'s fold: the held
/// operations, then the kept sources) refuses none of them and holds what the replayed index held.
#[test]
fn replay_admission_answers_holds_and_leaves_what_an_index_built_in_replay_would() {
    let pool = pool();
    let events: Vec<(Kind, &Header)> = pool
        .iter()
        .flat_map(|header| [(Kind::Operation, header), (Kind::Source, header)])
        .collect();
    let mut seen: Vec<Option<Refused>> = Vec::new();
    let mut sequences = 0;
    for capacity in 1..=3 {
        let mut frontier = vec![Vec::new()];
        for _ in 0..=4 {
            let mut next = Vec::new();
            for sequence in &frontier {
                sequences += 1;
                let mut index = Index::new(capacity);
                let mut want = BTreeMap::new();
                let expected = indexed(&mut index, &mut want, sequence);
                let mut causal = ReplayIndex::new(capacity);
                let mut held = BTreeMap::new();
                let mut kept = Vec::new();
                let got = admitted(&mut causal, &mut held, &mut kept, sequence);
                let named = || {
                    sequence
                        .iter()
                        .map(|(kind, header)| {
                            let kind = match kind {
                                Kind::Operation => "op",
                                Kind::Source => "src",
                            };
                            format!("{kind}({},{})", header.id[0], header.actor[0])
                        })
                        .collect::<Vec<_>>()
                        .join(" ")
                };
                assert_eq!(
                    got,
                    expected,
                    "first error, capacity {capacity}: {}",
                    named()
                );
                assert_eq!(held, want, "held, capacity {capacity}: {}", named());
                let mut lazy = Index::new(capacity);
                for header in held.values().chain(&kept) {
                    assert!(
                        lazy.insert(header.clone()).is_ok(),
                        "the lazy fold refused a header, capacity {capacity}: {}",
                        named()
                    );
                }
                assert_eq!(
                    content(&lazy),
                    content(&index),
                    "index content, capacity {capacity}: {}",
                    named()
                );
                if !seen.contains(&expected.err()) {
                    seen.push(expected.err());
                }
                if sequence.len() < 4 {
                    for event in &events {
                        let mut longer = sequence.clone();
                        longer.push(*event);
                        next.push(longer);
                    }
                }
            }
            frontier = next;
        }
    }
    assert_eq!(sequences, 3 * (1 + 10 + 100 + 1_000 + 10_000));
    let mut every = vec![None, Some(("duplicate physical table record", None))];
    for message in [
        "invalid discarded-source header",
        "invalid table replay header",
    ] {
        for refusal in [Refusal::Shape, Refusal::DigestCollision, Refusal::Capacity] {
            every.push(Some((message, Some(refusal))));
        }
    }
    assert!(
        seen.len() == every.len() && every.iter().all(|answer| seen.contains(answer)),
        "the pool reaches every answer: {seen:?}"
    );
}
