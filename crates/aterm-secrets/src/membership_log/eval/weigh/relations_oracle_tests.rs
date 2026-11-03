// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Independent relation and grounded-verdict checks against the previous set representation.

use super::{Relations, grounded};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
struct Oracle {
    undoes: BTreeSet<(usize, usize)>,
    strips: BTreeSet<(usize, usize)>,
}

impl Oracle {
    /// The original BTreeSet grounder, including its ascending edges and LIFO worklist.
    fn grounded(&self, apart: &[usize]) -> Vec<usize> {
        let at: BTreeMap<usize, usize> = apart.iter().enumerate().map(|(i, &q)| (q, i)).collect();
        let mut defeated: Vec<Vec<usize>> = vec![Vec::new(); apart.len()];
        let mut standing = vec![0usize; apart.len()];
        for &(one, other) in &self.undoes {
            let defeats = !self.undoes.contains(&(other, one))
                || self.strips.contains(&(one, other)) && !self.strips.contains(&(other, one));
            if defeats {
                defeated[at[&one]].push(at[&other]);
                standing[at[&other]] += 1;
            }
        }
        let mut label: Vec<Option<bool>> = vec![None; apart.len()];
        let mut found_in: Vec<usize> = (0..apart.len()).filter(|&i| standing[i] == 0).collect();
        for &i in &found_in {
            label[i] = Some(true);
        }
        while let Some(i) = found_in.pop() {
            for &out in &defeated[i] {
                if label[out].is_some() {
                    continue;
                }
                label[out] = Some(false);
                for &next in &defeated[out] {
                    standing[next] -= 1;
                    if standing[next] == 0 && label[next].is_none() {
                        label[next] = Some(true);
                        found_in.push(next);
                    }
                }
            }
        }
        apart
            .iter()
            .zip(&label)
            .filter(|&(_, label)| *label != Some(false))
            .map(|(&q, _)| q)
            .collect()
    }
}

fn packed(apart: &[usize], oracle: &Oracle, reverse: bool) -> Relations {
    let mut relations = Relations::new(apart).unwrap();
    let insert = |relations: &mut Relations, &(one, other): &(usize, usize), undo| {
        if undo {
            relations.set_undoes(one, other);
            relations.set_undoes(one, other);
        } else {
            relations.set_strips(one, other);
            relations.set_strips(one, other);
        }
    };
    for (pairs, undo) in [(&oracle.undoes, true), (&oracle.strips, false)] {
        if reverse {
            for pair in pairs.iter().rev() {
                insert(&mut relations, pair, undo);
            }
        } else {
            for pair in pairs {
                insert(&mut relations, pair, undo);
            }
        }
    }
    relations
}

fn check(apart: &[usize], oracle: &Oracle, reverse: bool) {
    let relations = packed(apart, oracle, reverse);
    assert_eq!(
        relations.undoes_pairs().collect::<Vec<_>>(),
        oracle.undoes.iter().copied().collect::<Vec<_>>(),
        "undo relations, reversed insertion={reverse}"
    );
    assert_eq!(
        relations.strips_pairs().collect::<Vec<_>>(),
        oracle.strips.iter().copied().collect::<Vec<_>>(),
        "strip relations, reversed insertion={reverse}"
    );
    for &one in apart {
        let expected_undoes: Vec<_> = oracle
            .undoes
            .range((one, 0)..=(one, usize::MAX))
            .map(|&(_, other)| other)
            .collect();
        let expected_strips: Vec<_> = oracle
            .strips
            .range((one, 0)..=(one, usize::MAX))
            .map(|&(_, other)| other)
            .collect();
        assert_eq!(
            relations.undoes_from(one).collect::<Vec<_>>(),
            expected_undoes
        );
        assert_eq!(
            relations.strips_from(one).collect::<Vec<_>>(),
            expected_strips
        );
        for &other in apart {
            assert_eq!(
                relations.undoes(one, other),
                oracle.undoes.contains(&(one, other))
            );
            assert_eq!(
                relations.strips(one, other),
                oracle.strips.contains(&(one, other))
            );
        }
    }
    assert_eq!(grounded(apart, &relations), oracle.grounded(apart));
}

#[test]
fn every_three_candidate_relation_matches_the_original_grounder() {
    let apart = [2, 17, 93];
    let pairs: Vec<_> = apart
        .iter()
        .flat_map(|&one| {
            apart
                .iter()
                .copied()
                .filter(move |&other| other != one)
                .map(move |other| (one, other))
        })
        .collect();
    assert_eq!(pairs.len(), 6);
    // Each of the six directed pairs has independent undo and strip bits. This includes
    // strips without undo, mutual ties, every three-cycle and every asymmetric strip choice.
    for mask in 0u16..4096 {
        let mut oracle = Oracle::default();
        for (bit, &pair) in pairs.iter().enumerate() {
            if mask & (1 << bit) != 0 {
                oracle.undoes.insert(pair);
            }
            if mask & (1 << (bit + pairs.len())) != 0 {
                oracle.strips.insert(pair);
            }
        }
        check(&apart, &oracle, false);
        check(&apart, &oracle, true);
    }
}

#[test]
fn self_relations_are_retained_without_defeating_their_own_candidate() {
    let apart = [2, 17, 93];
    let bases: [&[(usize, usize)]; 3] = [&[], &[(2, 17), (17, 93)], &[(2, 17), (17, 93), (93, 2)]];
    for base in bases {
        let expected = Oracle {
            undoes: base.iter().copied().collect(),
            strips: BTreeSet::new(),
        }
        .grounded(&apart);
        for mask in 0u8..64 {
            let mut oracle = Oracle {
                undoes: base.iter().copied().collect(),
                strips: BTreeSet::new(),
            };
            for (bit, &candidate) in apart.iter().enumerate() {
                if mask & (1 << bit) != 0 {
                    oracle.undoes.insert((candidate, candidate));
                }
                if mask & (1 << (bit + apart.len())) != 0 {
                    oracle.strips.insert((candidate, candidate));
                }
            }
            assert_eq!(oracle.grounded(&apart), expected);
            check(&apart, &oracle, false);
            check(&apart, &oracle, true);
        }
    }
}

#[test]
fn noncontiguous_candidate_rows_and_verdicts_cross_word_boundaries() {
    for count in [0, 1, 2, 63, 64, 65, 127, 128, 129] {
        let apart: Vec<_> = (0..count).map(|i| 3 * i + 5).collect();
        let mut oracle = Oracle::default();
        for (i, &one) in apart.iter().enumerate() {
            for (j, &other) in apart.iter().enumerate() {
                if i == j {
                    continue;
                }
                if (i * 17 + j * 31) % 7 < 3 || i.abs_diff(j) == 64 {
                    oracle.undoes.insert((one, other));
                }
                if (i * 11 + j * 19) % 5 < 2 {
                    oracle.strips.insert((one, other));
                }
            }
        }
        check(&apart, &oracle, false);
        check(&apart, &oracle, true);
    }
}

#[test]
fn maximum_current_candidate_layout_preserves_long_grounding_propagation() {
    let apart: Vec<_> = (0..2047).collect();
    let mut oracle = Oracle::default();
    // A long directed chain forces the old worklist's alternating in/out propagation across
    // every packed row and word. At each word edge, only the forward direction strips;
    // the added reciprocal undo preserves the chain's independently known alternating verdict.
    for pair in apart.windows(2) {
        oracle.undoes.insert((pair[0], pair[1]));
        if pair[0] % 64 == 63 {
            oracle.undoes.insert((pair[1], pair[0]));
            oracle.strips.insert((pair[0], pair[1]));
        }
    }
    let relations = packed(&apart, &oracle, true);
    let expected: Vec<_> = apart.iter().step_by(2).copied().collect();
    assert_eq!(oracle.grounded(&apart), expected);
    assert_eq!(grounded(&apart, &relations), expected);
    assert_eq!(
        relations.undoes_pairs().collect::<Vec<_>>(),
        oracle.undoes.iter().copied().collect::<Vec<_>>()
    );
    assert_eq!(
        relations.strips_pairs().collect::<Vec<_>>(),
        oracle.strips.iter().copied().collect::<Vec<_>>()
    );
}
