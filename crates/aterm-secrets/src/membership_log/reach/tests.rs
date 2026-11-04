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
fn random_parents(seed: u64) -> Vec<Vec<usize>> {
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

fn indexed(parents: &[Vec<usize>]) -> Reach {
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
/// row copied and unioned over a set that starts full of another row, `in_past_of` and
/// `past_intersects` probes, and closures of a few roots.
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
        let mut copy = vec![u64::MAX; words];
        reach.copy_past_into(b, &mut copy);
        wrong += (0..n).filter(|&a| has(&copy, a) != row[a]).count();
        let other = rng.below(n);
        let mut union = vec![0; words];
        reach.copy_past_into(other, &mut union);
        reach.union_past_into(b, &mut union);
        wrong += (0..n)
            .filter(|&a| has(&union, a) != (row[a] || past[other][a]))
            .count();
        let probe = rng.below(n);
        let mut one = vec![0; words];
        one[probe / 64] |= 1 << (probe % 64);
        wrong += usize::from(reach.past_intersects(b, &one) != row[probe]);
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
