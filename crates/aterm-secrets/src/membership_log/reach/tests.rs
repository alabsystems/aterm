// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The index answers what a depth-first walk of the parents answers, on seeded random graphs,
//! and its memory, work and sharing are pinned by count.

use super::*;

/// A small xorshift generator: the same seed, the same graphs.
pub(in crate::membership_log) struct Rng(pub(in crate::membership_log) u64);

impl Rng {
    pub(in crate::membership_log) fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    pub(in crate::membership_log) fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(n).unwrap()).unwrap()
    }
}

/// A random history from `seed`: up to 320 records (so rows cross several word and chunk
/// boundaries), each a chain step, a fork from any earlier record, a fork at the genesis (what a
/// Mac's records at sequence one give), a merge of up to 64 earlier records, or a step that also
/// follows one earlier record. Each record's parents are sorted and distinct, as a signed
/// record's are.
pub(in crate::membership_log) fn random_parents(seed: u64) -> Vec<Vec<usize>> {
    let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    let n = 1 + rng.below(320);
    let mut parents = vec![Vec::new()];
    for i in 1..n {
        let roll = rng.below(100);
        let mut mine = if roll < 35 {
            vec![i - 1]
        } else if roll < 50 {
            vec![rng.below(i)]
        } else if roll < 65 {
            vec![0]
        } else if roll < 85 {
            let k = (2 + rng.below(i.clamp(2, 64) - 1)).min(i);
            let mut picked = std::collections::BTreeSet::new();
            while picked.len() < k {
                picked.insert(rng.below(i));
            }
            picked.into_iter().collect()
        } else {
            vec![i - 1, rng.below(i)]
        };
        mine.sort_unstable();
        mine.dedup();
        mine.truncate(64);
        parents.push(mine);
    }
    parents
}

/// Each record's strict past, by a depth-first walk of `parents`.
pub(in crate::membership_log) fn walked(parents: &[Vec<usize>]) -> Vec<Vec<bool>> {
    parents
        .iter()
        .map(|incoming| {
            let mut seen = vec![false; parents.len()];
            let mut stack = incoming.clone();
            while let Some(p) = stack.pop() {
                if !seen[p] {
                    seen[p] = true;
                    stack.extend(&parents[p]);
                }
            }
            seen
        })
        .collect()
}

pub(in crate::membership_log) fn indexed(parents: &[Vec<usize>]) -> Reach {
    let mut reach = Reach::default();
    for incoming in parents {
        reach.push(incoming.iter().copied());
    }
    reach
}

fn has(set: &[u64], a: usize) -> bool {
    (set[a / 64] >> (a % 64)) & 1 == 1
}

/// How many of the index's answers differ from the walk's: every `before` and `upto` pair, each
/// row unioned into an empty set and into another record's closure, `in_past_of` probes, and
/// closures of a few roots.
pub(in crate::membership_log) fn disagreements(
    past: &[Vec<bool>],
    reach: &Reach,
    rng: &mut Rng,
) -> usize {
    let n = past.len();
    let words = n.div_ceil(64);
    let mut wrong = usize::from(reach.len() != n);
    for (b, row) in past.iter().enumerate() {
        for (a, &earlier) in row.iter().enumerate() {
            wrong += usize::from(reach.before(a, b) != earlier);
            wrong += usize::from(reach.upto(a, b) != (a == b || earlier));
        }
        wrong += usize::from(reach.before(b, n) || reach.upto(n, n));
        let mut alone = vec![0; words];
        reach.union_past_into(b, &mut alone);
        wrong += (0..n).filter(|&a| has(&alone, a) != row[a]).count();
        let other = rng.below(n);
        let mut union = reach.closure([other]);
        reach.union_past_into(b, &mut union);
        wrong += (0..n)
            .filter(|&a| has(&union, a) != (row[a] || a == other || past[other][a]))
            .count();
        let probe = rng.below(n);
        let after: Vec<usize> = (0..1 + rng.below(3)).map(|_| rng.below(n)).collect();
        let want = after.iter().any(|&p| p == probe || past[p][probe]);
        wrong += usize::from(reach.in_past_of(probe, &after) != want);
        let closure = reach.closure(after.iter().copied());
        wrong += (0..n)
            .filter(|&a| has(&closure, a) != after.iter().any(|&p| p == a || past[p][a]))
            .count();
    }
    wrong
}

/// The index answers what a depth-first walk answers on 200 seeded histories with forks, merges
/// of up to 64 parents and forks at the genesis.
/// NEGATIVE CONTROLS: rows that leave out each record's first parent's row, or that leave its
/// first parent's own bit unset, disagree with the walk.
#[test]
fn two_hundred_random_histories_match_a_depth_first_walk() {
    let histories: Vec<_> = (1..=200).map(random_parents).collect();
    let pasts: Vec<_> = histories.iter().map(|parents| walked(parents)).collect();
    let widest = histories.iter().flatten().map(Vec::len).max().unwrap();
    let longest = histories.iter().map(Vec::len).max().unwrap();
    assert_eq!(widest, 64, "some merge has 64 parents");
    assert!(longest > 4 * ROWS, "some history crosses four chunks");
    for mutant in [
        None,
        Some(Mutant::OmitParentRow),
        Some(Mutant::OmitParentBit),
    ] {
        let mut wrong = 0;
        for ((seed, parents), past) in (1..).zip(&histories).zip(&pasts) {
            MUTANT.set(mutant);
            let reach = indexed(parents);
            MUTANT.set(None);
            wrong += disagreements(past, &reach, &mut Rng(seed));
        }
        match mutant {
            None => assert_eq!(wrong, 0, "the index is exact"),
            Some(mutant) => assert!(wrong > 0, "{mutant:?} goes unnoticed"),
        }
    }
}

/// The rows take exactly the triangular bytes: 16,840,704 (16.06 MiB) at 16,384 records, the sum
/// over every record of `ceil(i / 64) * 8`; a partial last chunk reserves the whole chunk and no
/// more; and indexing a chain ORs exactly each parent's row, `ceil(j / 64)` words for parent `j`.
#[test]
fn rows_take_exactly_the_triangular_bytes_and_a_chain_its_parents_words() {
    let sum = |n: usize| -> usize { (0..n).map(|i| i.div_ceil(64) * 8).sum() };
    for n in [0, 1, 2, 63, 64, 65, 127, 128, 129, 1000, 4096, 16_384] {
        assert_eq!(bytes_for(n), sum(n), "{n} records");
        assert_eq!(reserved_for(n), sum(n.div_ceil(64) * 64), "{n} records");
    }
    assert_eq!(bytes_for(16_384), 16_840_704);
    let n: usize = 16_384;
    let mut reach = Reach::default();
    for i in 0..n {
        reach.push(i.checked_sub(1));
        if i == 99 {
            assert_eq!(reach.bytes(), reserved_for(100));
            assert!(reach.bytes() - bytes_for(100) < full_words(1) * 8);
        }
    }
    assert_eq!(reach.bytes(), bytes_for(n));
    let ors: usize = (1..n).map(|i| (i - 1).div_ceil(64)).sum();
    assert_eq!(reach.cost.word_ors, ors);
    assert_eq!(reach.cost.chunk_copies, 0);
    assert!(reach.before(0, n - 1) && reach.before(n - 2, n - 1));
    assert!(!reach.before(n - 1, n - 1));
}

/// A clone shares every chunk; the first record it indexes into a shared partial chunk copies
/// that chunk alone, and no other, while the original's rows stay as they were; one that starts
/// a chunk copies nothing.
#[test]
fn clones_share_their_chunks_and_an_append_copies_at_most_one() {
    let mut parents = random_parents(7);
    while parents.len() < 200 {
        let i = parents.len();
        parents.push(vec![i - 1]);
    }
    parents.truncate(200);
    let original = indexed(&parents);
    let mut clone = original.clone();
    assert_eq!(clone.chunks(), 4);
    assert!((0..4).all(|c| clone.shares_chunk(&original, c)));
    clone.push([199, 3]);
    assert_eq!(clone.cost.chunk_copies, 1);
    assert!(clone.cost.copied_words <= full_words(3));
    assert!((0..3).all(|c| clone.shares_chunk(&original, c)));
    assert!(!clone.shares_chunk(&original, 3));
    assert!(clone.before(3, 200) && !original.before(3, 200));
    assert_eq!(disagreements(&walked(&parents), &original, &mut Rng(1)), 0);
    clone.push([200]);
    assert_eq!(clone.cost.chunk_copies, 1, "its own chunk is not shared");

    parents.truncate(192);
    let original = indexed(&parents);
    let mut clone = original.clone();
    clone.push([191]);
    assert_eq!(clone.cost.chunk_copies, 0, "a new chunk copies nothing");
    assert!((0..3).all(|c| clone.shares_chunk(&original, c)));
}

/// The row words a closure of `roots` ORs, and the set it gives.
fn closure_cost(reach: &Reach, roots: &[usize]) -> (usize, Vec<u64>) {
    CLOSURE_ORS.set(0);
    let set = reach.closure(roots.iter().copied());
    (CLOSURE_ORS.get(), set)
}

/// A closure ORs one row for each distinct root that lies in no other root's past: 4,096 copies
/// of a chain's head, or every record of the chain listed lowest first, cost the head's 64 words
/// alone, and on 200 seeded histories a few random roots, repeated, cost the rows of those no
/// other root follows, while every closure answers what the walk does.
/// NEGATIVE CONTROL: a closure that unions every root's row reads the head's row once per copy.
#[test]
fn a_closure_reads_one_row_for_each_root_no_other_root_follows() {
    let n: usize = 4_096;
    let chain: Vec<Vec<usize>> = (0..n)
        .map(|i| i.checked_sub(1).into_iter().collect())
        .collect();
    let reach = indexed(&chain);
    let head = n - 1;
    let row = head.div_ceil(64);
    assert_eq!(row, 64);
    let repeats = vec![head; n];
    let every: Vec<usize> = (0..n).collect();
    for roots in [&repeats, &every] {
        let (ors, set) = closure_cost(&reach, roots);
        assert_eq!(ors, row);
        assert!((0..n).all(|a| has(&set, a)));
    }
    MUTANT.set(Some(Mutant::UnionEveryRoot));
    let (ors, _) = closure_cost(&reach, &repeats);
    MUTANT.set(None);
    assert_eq!(ors, n * row, "unioning every root goes unnoticed");

    let mut uncovered = 0;
    for seed in 1..=200 {
        let parents = random_parents(seed);
        let past = walked(&parents);
        let reach = indexed(&parents);
        let mut rng = Rng(seed);
        let n = parents.len();
        let picked: Vec<usize> = (0..1 + rng.below(6)).map(|_| rng.below(n)).collect();
        let roots: Vec<usize> = picked.iter().flat_map(|&r| [r, r]).collect();
        let tops: std::collections::BTreeSet<usize> = picked
            .iter()
            .copied()
            .filter(|&r| !picked.iter().any(|&other| past[other][r]))
            .collect();
        uncovered += tops.len();
        let (ors, set) = closure_cost(&reach, &roots);
        assert_eq!(
            ors,
            tops.iter().map(|&r| r.div_ceil(64)).sum::<usize>(),
            "seed {seed}"
        );
        let held = |a: usize| picked.iter().any(|&r| r == a || past[r][a]);
        let wrong = (0..n).filter(|&a| has(&set, a) != held(a)).count();
        assert_eq!(wrong, 0, "seed {seed}");
    }
    assert!(
        uncovered > 200,
        "some histories had several roots no other followed"
    );
}

/// The newest of `set` by testing each pair against the walk's pasts.
fn newest_walked(past: &[Vec<bool>], set: &[usize]) -> Vec<usize> {
    set.iter()
        .copied()
        .filter(|&a| !set.iter().any(|&b| past[b][a]))
        .collect()
}

/// A random set of distinct records of `n`, ascending.
fn random_set(rng: &mut Rng, n: usize, most: usize) -> Vec<usize> {
    let mut set: Vec<usize> = (0..1 + rng.below(most)).map(|_| rng.below(n)).collect();
    set.sort_unstable();
    set.dedup();
    set
}

/// The newest of a set are what testing each pair against a depth-first walk keeps, on 200
/// seeded histories with sets of up to 80 records, and finding them costs a bit test per record
/// and, per newest record, its row from the lowest record's word on.
/// NEGATIVE CONTROL: keeping a record a newer one's row already holds keeps records the walk
/// puts in another's past.
#[test]
fn the_newest_of_a_set_match_a_depth_first_walk() {
    let mut newest_seen = 0;
    for mutant in [None, Some(Mutant::KeepCovered)] {
        let mut wrong = 0;
        for seed in 1..=200 {
            let parents = random_parents(seed);
            let past = walked(&parents);
            let reach = indexed(&parents);
            let mut rng = Rng(seed);
            for _ in 0..8 {
                let set = random_set(&mut rng, parents.len(), 80);
                let want = newest_walked(&past, &set);
                MUTANT.set(mutant);
                WORK.set(Work::default());
                let got = reach.maximal(&set);
                let work = WORK.get();
                MUTANT.set(None);
                wrong += usize::from(got != want);
                if mutant.is_none() {
                    newest_seen += want.len();
                    let base = set[0] / 64;
                    let rows: usize = want
                        .iter()
                        .map(|&m| m.div_ceil(64).saturating_sub(base))
                        .sum();
                    assert_eq!(work.bits, set.len(), "seed {seed}");
                    assert_eq!(work.words, rows, "seed {seed}");
                }
            }
        }
        match mutant {
            None => assert_eq!(wrong, 0, "the newest are exact"),
            Some(mutant) => assert!(wrong > 0, "{mutant:?} goes unnoticed"),
        }
    }
    assert!(newest_seen > 1_600, "sets had several newest records");
}

/// Each grouped record's frontier, by the walk: the records of its group in its strict past
/// that lie in no other such record's past, as positions within the group.
fn frontier_walked(past: &[Vec<bool>], group: &[usize], position: usize) -> Vec<usize> {
    let x = group[position];
    let below: Vec<usize> = (0..position).filter(|&j| past[x][group[j]]).collect();
    below
        .iter()
        .copied()
        .filter(|&j| !below.iter().any(|&other| past[group[other]][group[j]]))
        .collect()
}

/// Up to six groups over a random history's records past the genesis, some large enough to be
/// read through a mask, the rest left out.
fn random_groups(rng: &mut Rng, n: usize) -> Vec<Vec<usize>> {
    let count = 1 + rng.below(6);
    let mut groups = vec![Vec::new(); count];
    let share = 1 + rng.below(4);
    for x in 1..n {
        if rng.below(4) < share {
            groups[rng.below(count)].push(x);
        }
    }
    groups
}

/// How many frontiers the pass hands out on `parents` with `groups` differ from the walk's, and
/// whether it handed them out in record order, each grouped record once.
fn frontier_disagreements(parents: &[Vec<usize>], groups: &[Vec<usize>]) -> (usize, usize) {
    let past = walked(parents);
    let reach = indexed(parents);
    let mut visited = Vec::new();
    let mut wrong = 0;
    reach.frontiers(
        groups,
        |x| parents[x].iter().copied(),
        |g, position, frontier| {
            visited.push(groups[g][position]);
            wrong +=
                usize::from(frontier.positions() != frontier_walked(&past, &groups[g], position));
        },
    );
    let mut every: Vec<usize> = groups.iter().flatten().copied().collect();
    every.sort_unstable();
    assert_eq!(visited, every, "each grouped record once, in record order");
    (wrong, visited.len())
}

/// The pass hands each grouped record the frontier a depth-first walk gives, in record order, on
/// 200 seeded histories with up to six groups each (masked groups of 64 records or more and
/// bit-tested smaller ones), and on a fork of 300 records under one root that one merge tree
/// joins and 300 more follow.
/// NEGATIVE CONTROLS: a frontier that ignores what the parents dominate, or a dominated set that
/// leaves out the records of a grouped record's own group in its past, disagree with the walk.
#[test]
fn every_frontier_matches_a_depth_first_walk() {
    let mut cases: Vec<_> = (1..=200)
        .map(|seed| {
            let parents = random_parents(seed);
            let groups = random_groups(&mut Rng(seed ^ 0x5eed), parents.len());
            (parents, groups)
        })
        .collect();
    let (fan, ops) = fanned(300, 300);
    cases.push((fan, vec![ops]));
    assert!(
        cases
            .iter()
            .any(|(_, groups)| groups.iter().any(|g| g.len() >= 64)),
        "some group is masked"
    );
    for mutant in [
        None,
        Some(Mutant::IgnoreParents),
        Some(Mutant::OmitOwnGroup),
    ] {
        let mut wrong = 0;
        let mut visited = 0;
        for (parents, groups) in &cases {
            MUTANT.set(mutant);
            let (bad, seen) = frontier_disagreements(parents, groups);
            MUTANT.set(None);
            wrong += bad;
            visited += seen;
        }
        assert!(visited > 10_000, "the groups held records");
        match mutant {
            None => assert_eq!(wrong, 0, "every frontier is exact"),
            Some(mutant) => assert!(wrong > 0, "{mutant:?} goes unnoticed"),
        }
    }
}

/// A root, `lower` records that each follow it alone (forks, as one Mac's records at one
/// sequence are), merges of up to 64 of them that end in one record, and `upper` records that
/// each follow that one: the history every one of `upper` sees all of `lower` in, and the
/// grouped records (`lower` then `upper`).
pub(in crate::membership_log) fn fanned(
    lower: usize,
    upper: usize,
) -> (Vec<Vec<usize>>, Vec<usize>) {
    let mut parents: Vec<Vec<usize>> = vec![Vec::new()];
    let mut ops = Vec::new();
    for _ in 0..lower {
        ops.push(parents.len());
        parents.push(vec![0]);
    }
    let mut level: Vec<usize> = ops.clone();
    while level.len() > 1 {
        let mut next = Vec::new();
        for chunk in level.chunks(64) {
            next.push(parents.len());
            parents.push(chunk.to_vec());
        }
        level = next;
    }
    let top = level[0];
    for _ in 0..upper {
        ops.push(parents.len());
        parents.push(vec![top]);
    }
    (parents, ops)
}
