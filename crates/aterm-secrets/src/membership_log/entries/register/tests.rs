// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The register resolver gives what the one it replaced gave ([`oracle`]) on 200 seeded
//! histories with concurrent registers (on 200 signed logs, `entries`'s
//! `two_hundred_signed_logs_read_as_the_replaced_readings_did`); and what it costs is pinned by
//! count on one Mac's 12,288-op register, where the scan it replaced runs past the pin, on a
//! register of 12,288 records whose ops read growing frontiers of carries, its bound, and on a
//! frontier of reseals and edits, where each op's row is read over one word.

use super::super::super::reach::tests::{Rng, fanned, indexed, random_parents, walked};
use super::super::super::reach::{self, WORK, Work};
use super::super::{CandidateOp, DecodedOp, EntryMeta};
use super::oracle::{self, Exhausted};
use super::{MUTANT, Mutant, Register};
use crate::membership_log::RecordId;

/// An op at record `index`: a `Delete`, or a `Put` of one of `values` values, with an id from
/// `rng`.
fn op(rng: &mut Rng, index: usize, values: usize) -> CandidateOp {
    let roll = rng.below(values + 1);
    let op = if roll == values {
        DecodedOp::Delete
    } else {
        DecodedOp::Put(EntryMeta {
            index,
            name: String::new(),
            if_absent: false,
            anchor: None,
            value_class: roll,
        })
    };
    put_or_delete(rng, index, op)
}

fn put_or_delete(rng: &mut Rng, index: usize, op: DecodedOp) -> CandidateOp {
    let mut record_id: RecordId = [0; 32];
    for chunk in record_id.chunks_mut(8) {
        chunk.copy_from_slice(&rng.next().to_be_bytes());
    }
    CandidateOp {
        index,
        record_id,
        epoch: [0; 32],
        op,
    }
}

/// A `Put` of value `value` at record `index`.
fn put(rng: &mut Rng, index: usize, value: usize) -> CandidateOp {
    put_or_delete(
        rng,
        index,
        DecodedOp::Put(EntryMeta {
            index,
            name: String::new(),
            if_absent: false,
            anchor: None,
            value_class: value,
        }),
    )
}

/// Up to five registers over a random history's records past the genesis, of two or three
/// values and `Delete`s, so carries, replaced carries and conflicts all arise.
fn random_registers(rng: &mut Rng, n: usize) -> Vec<Vec<CandidateOp>> {
    let count = 1 + rng.below(5);
    let values = 2 + rng.below(2);
    let mut registers: Vec<Vec<CandidateOp>> = (0..count).map(|_| Vec::new()).collect();
    for x in 1..n {
        if rng.below(3) < 2 {
            let r = rng.below(count);
            registers[r].push(op(rng, x, values));
        }
    }
    registers
}

/// What the resolver gives each of `registers` over `parents`, and the work it took.
fn resolve(
    parents: &[Vec<usize>],
    registers: &[Vec<CandidateOp>],
) -> (Vec<oracle::Resolved>, Work) {
    let reach = indexed(parents);
    let ops: Vec<&[CandidateOp]> = registers.iter().map(Vec::as_slice).collect();
    WORK.set(Work::default());
    let resolved = Register::classify_all(&ops, &reach, |x| parents[x].iter().copied())
        .iter()
        .map(|register| oracle::resolved(register, &register.siblings(&reach)))
        .collect();
    (resolved, WORK.get())
}

/// How many of `registers` the resolver resolves otherwise than the replaced one, reading
/// causality from a depth-first walk of `parents`.
fn disagreements(parents: &[Vec<usize>], registers: &[Vec<CandidateOp>]) -> usize {
    let past = walked(parents);
    let (got, _) = resolve(parents, registers);
    got.iter()
        .zip(registers)
        .filter(|(got, cands)| {
            let (want, _) = oracle::resolve(cands, &|a, b| past[b][a], usize::MAX);
            want.as_ref() != Ok(*got)
        })
        .count()
}

/// Run `run` with the register mutant `register` and the index mutant `index`, clearing both
/// however `run` ends.
fn with_mutants<T>(
    register: Option<Mutant>,
    index: Option<reach::Mutant>,
    run: impl FnOnce() -> T,
) -> T {
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            MUTANT.set(None);
            reach::MUTANT.set(None);
        }
    }
    let _clear = Clear;
    MUTANT.set(register);
    reach::MUTANT.set(index);
    run()
}

/// On 200 seeded histories (forks, merges of up to 64 parents, forks at the genesis) with up to
/// five concurrent registers each, every op's carry and rep and every register's siblings are
/// what the replaced resolver gives, reading causality from a depth-first walk.
/// NEGATIVE CONTROLS: what a writer held keeping every carry, an op reading the last op's
/// frontier, and frontiers that ignore what the parents dominate each disagree.
#[test]
fn two_hundred_histories_resolve_as_the_replaced_resolver_did() {
    let cases: Vec<_> = (1..=200)
        .map(|seed| {
            let parents = random_parents(seed);
            let registers = random_registers(&mut Rng(seed ^ 0xc0ffee), parents.len());
            (parents, registers)
        })
        .collect();
    let ops: usize = cases.iter().flat_map(|(_, r)| r).map(Vec::len).sum();
    assert!(ops > 10_000, "the registers held ops");
    let carries: usize = cases
        .iter()
        .map(|(parents, registers)| {
            resolve(parents, registers)
                .0
                .iter()
                .map(|r| r.carries.iter().filter(|&&c| c).count())
                .sum::<usize>()
        })
        .sum();
    assert!(carries > 1_000, "carries arose: {carries}");
    for (register, index) in [
        (None, None),
        (Some(Mutant::KeepEveryCarry), None),
        (Some(Mutant::ReuseFrontier), None),
        (None, Some(reach::Mutant::IgnoreParents)),
    ] {
        let wrong: usize = with_mutants(register, index, || {
            cases
                .iter()
                .map(|(parents, registers)| disagreements(parents, registers))
                .sum()
        });
        match (register, index) {
            (None, None) => assert_eq!(wrong, 0, "the resolver is exact"),
            _ => assert!(wrong > 0, "{register:?} {index:?} goes unnoticed"),
        }
    }
}

/// The register size S10 lets one seated Mac reach in one register.
const K: usize = 12_288;

/// The pin: `O(K^2 / 64)` words, three per op's row in all.
const PIN: usize = 3 * K * K / 64;

/// The steps pin of a register whose ops read `O(K)` distinct frontiers of `O(K)` ops: a few
/// steps per op of each, `O(K^2)` (module docs).
const STEPS_PIN: usize = K * K / 2;

/// What resolving `registers` over `parents` costs, and what the replaced resolver did once its
/// tests passed `budget`.
fn pinned(
    parents: &[Vec<usize>],
    registers: &[Vec<CandidateOp>],
    budget: usize,
) -> (Work, Vec<Result<oracle::Resolved, Exhausted>>) {
    let (_, work) = resolve(parents, registers);
    let reach = indexed(parents);
    let old = registers
        .iter()
        .map(|cands| oracle::resolve(cands, &|a, b| reach.before(a, b), budget).0)
        .collect();
    (work, old)
}

/// Join the records of `level` with merge trees of at most 64 parents a record, appended to
/// `parents`; the root.
fn joined(parents: &mut Vec<Vec<usize>>, mut level: Vec<usize>) -> usize {
    while level.len() > 1 {
        let mut next = Vec::new();
        for chunk in level.chunks(64) {
            next.push(parents.len());
            parents.push(chunk.to_vec());
        }
        level = next;
    }
    level[0]
}

/// A chain of `K / 3` writes of two values, a reseal forked off each, a merge of every reseal
/// and one of all but the first, and `K / 3` writes alternating between the two merges: the
/// records' parents, and the register's ops.
fn alternating_merges(rng: &mut Rng) -> (Vec<Vec<usize>>, Vec<CandidateOp>) {
    let third = K / 3;
    let mut parents: Vec<Vec<usize>> = (0..=third)
        .map(|i| i.checked_sub(1).into_iter().collect())
        .collect();
    let mut ops: Vec<CandidateOp> = (1..=third).map(|x| put(rng, x, x % 2)).collect();
    let mut reseals = Vec::new();
    for x in 1..=third {
        let at = parents.len();
        parents.push(vec![x]);
        ops.push(put(rng, at, x % 2));
        reseals.push(at);
    }
    let every = joined(&mut parents, reseals.clone());
    let all_but_first = joined(&mut parents, reseals[1..].to_vec());
    for i in 0..third {
        let at = parents.len();
        parents.push(vec![if i % 2 == 0 { every } else { all_but_first }]);
        ops.push(put(rng, at, 2));
    }
    (parents, ops)
}

/// One Mac's 12,288-op register costs `O(K^2 / 64)` words: a chain of `Put`s each on the last
/// (two values, so some carry); 6,144 forked `Put`s that one merge tree joins and 6,144 more
/// forked on it; 6,000 reseals and 6,000 edits of one value forked on it with 288 `Put`s on
/// their merge; and a chain of 4,096 `Put`s of two values, a reseal forked off each, a merge of
/// every reseal and one of all but the first, and 4,096 `Put`s alternating between the two
/// merges. Each stays within three row reads per op, 7,077,888 words and steps, while the scan
/// it replaced (testing each earlier op against the newest found so far) runs past that count
/// on each. 6,144 forked `Put`s and 6,144 more, the `i`-th on a merge of the first `i`, read
/// 6,144 distinct frontiers that grow by one op each: their words stay within the pin and their
/// steps within [`STEPS_PIN`], three per op of each frontier, where the replaced scan runs past
/// that.
/// NEGATIVE CONTROL: keeping what a writer held for the last frontier read alone (enough for
/// ops forked from one view) resolves the alternating shape alike, past the pin.
#[test]
fn one_macs_12288_op_register_costs_a_few_rows_per_op() {
    let mut rng = Rng(12_288);
    let chain_parents: Vec<Vec<usize>> = (0..=K)
        .map(|i| i.checked_sub(1).into_iter().collect())
        .collect();
    let chain: Vec<CandidateOp> = (1..=K)
        .map(|x| {
            let value = rng.below(2);
            put(&mut rng, x, value)
        })
        .collect();

    let (fan_parents, fan_ops) = fanned(K / 2, K / 2);
    let fan: Vec<CandidateOp> = fan_ops.iter().map(|&x| put(&mut rng, x, 0)).collect();

    // A root write, 6,000 reseals of it and 6,000 edits on it, all forked from it, joined by one
    // merge tree, and 288 writes on the merge.
    let (mut mixed_parents, lower) = fanned(1, 0);
    let root = lower[0];
    let mut mixed = vec![put(&mut rng, root, 0)];
    let mut level = Vec::new();
    for i in 0..12_000 {
        let x = mixed_parents.len();
        mixed_parents.push(vec![root]);
        mixed.push(put(&mut rng, x, usize::from(i % 2 == 1)));
        level.push(x);
    }
    let merge = joined(&mut mixed_parents, level);
    for _ in 0..K - 12_001 {
        let x = mixed_parents.len();
        mixed_parents.push(vec![merge]);
        mixed.push(put(&mut rng, x, 2));
    }
    assert_eq!(mixed.len(), K);

    let (alternating_parents, alternating) = alternating_merges(&mut Rng(4_096));
    assert_eq!(alternating.len(), K);

    for (name, parents, ops) in [
        ("chain", &chain_parents, chain),
        ("fan", &fan_parents, fan),
        ("reseals and edits", &mixed_parents, mixed),
        ("alternating merges", &alternating_parents, alternating),
    ] {
        assert_eq!(ops.len(), K, "{name}");
        let registers = vec![ops];
        let (work, old) = pinned(parents, &registers, PIN);
        eprintln!("register {name}: {work:?}, pin {PIN}");
        assert!(work.total() <= PIN, "{name}: {work:?} past {PIN}");
        assert_eq!(
            old,
            vec![Err(Exhausted)],
            "{name}: the replaced scan stays within the pin"
        );
    }
    let registers = vec![alternating_merges(&mut Rng(4_096)).1];
    let (got, work) = resolve(&alternating_parents, &registers);
    let (wrong, last) = with_mutants(Some(Mutant::RememberLast), None, || {
        resolve(&alternating_parents, &registers)
    });
    assert_eq!(
        got, wrong,
        "remembering the last frontier alone changes no verdict"
    );
    assert!(
        work.total() <= PIN && last.total() > PIN,
        "remembering the last frontier alone stays within the pin: {last:?}"
    );

    // Forked writes, and writes each on a merge of one more of them.
    let half = K / 2;
    let mut growing_parents: Vec<Vec<usize>> = vec![Vec::new()];
    let mut growing: Vec<CandidateOp> = Vec::new();
    for x in 1..=half {
        growing_parents.push(vec![0]);
        growing.push(put(&mut rng, x, x % 2));
    }
    let mut prefix = 0;
    for x in 1..=half {
        let merge = growing_parents.len();
        growing_parents.push(vec![prefix.min(x), prefix.max(x)]);
        prefix = merge;
        let at = growing_parents.len();
        growing_parents.push(vec![prefix]);
        growing.push(put(&mut rng, at, 2));
    }
    assert_eq!(growing.len(), K);
    let registers = vec![growing];
    let (work, old) = pinned(&growing_parents, &registers, STEPS_PIN);
    eprintln!("register growing frontiers: {work:?}, pin {PIN}, steps pin {STEPS_PIN}");
    assert!(
        work.words + work.bits <= PIN,
        "growing frontiers: {work:?}, words past {PIN}"
    );
    assert!(
        work.steps <= STEPS_PIN,
        "growing frontiers: {work:?}, steps past {STEPS_PIN}"
    );
    assert_eq!(
        old,
        vec![Err(Exhausted)],
        "growing frontiers: the replaced scan stays within the steps pin"
    );
}

/// `m` rounds on a genesis: a `Put` of one of two values on the genesis, its reseal on it (a
/// carry whose rep it is), a record joining the reseal to the last round's join, and a `Put` of
/// a third value on that join. Each round's last `Put` reads a frontier of every reseal so far,
/// carries of distinct reps beside carries of the other value. The records' parents, the
/// register's ops, and what reading those frontiers costs past [`PIN`]: for each, a row per op
/// over the words holding its carries' reps.
fn growing_carries(rng: &mut Rng, m: usize) -> (Vec<Vec<usize>>, Vec<CandidateOp>, usize) {
    let mut parents: Vec<Vec<usize>> = vec![Vec::new()];
    let mut ops = Vec::new();
    let mut join: Option<usize> = None;
    let (mut rep_words, mut last_word, mut reads) = (0, None, 0);
    for t in 0..m {
        let write = parents.len();
        parents.push(vec![0]);
        ops.push(put(rng, write, t % 2));
        let reseal = parents.len();
        parents.push(vec![write]);
        ops.push(put(rng, reseal, t % 2));
        let at = parents.len();
        parents.push(join.into_iter().chain([reseal]).collect());
        join = Some(at);
        let edit = parents.len();
        parents.push(vec![at]);
        ops.push(put(rng, edit, 2));
        if last_word != Some(write / 64) {
            last_word = Some(write / 64);
            rep_words += 1;
        }
        reads += (t + 1) * rep_words;
    }
    (parents, ops, reads)
}

/// One Mac's register of 3,072 such rounds, 12,288 records and 9,216 ops: each round's last op
/// reads a new frontier of reseals of distinct reps, which of them another value replaced turns
/// on that frontier as a whole, and each is read afresh (module docs). This is the resolver's
/// accepted bound, `O(K^3 / 64)` words: its words stay within [`PIN`] plus a row per op of
/// each such frontier over the words holding its reps (about 6e8 at `K`), and its steps within
/// [`STEPS_PIN`]. At 256 rounds every verdict is the replaced resolver's.
/// NEGATIVE CONTROL: the scan it replaced runs past that word count on the same input.
#[test]
fn growing_frontiers_of_carries_cost_a_row_per_op_over_their_reps_words() {
    let (parents, ops, _) = growing_carries(&mut Rng(256), 256);
    let registers = vec![ops];
    let (got, _) = resolve(&parents, &registers);
    assert_eq!(got[0].carries.iter().filter(|&&c| c).count(), 256);
    assert_eq!(disagreements(&parents, &registers), 0);

    let (parents, ops, reads) = growing_carries(&mut Rng(3_072), K / 4);
    assert_eq!(parents.len(), K + 1);
    let registers = vec![ops];
    let bound = PIN + reads;
    let (work, old) = pinned(&parents, &registers, bound);
    eprintln!("register growing carries: {work:?}, bound {bound}, steps pin {STEPS_PIN}");
    assert!(
        work.words + work.bits <= bound,
        "growing carries: {work:?}, words past {bound}"
    );
    assert!(
        work.steps <= STEPS_PIN,
        "growing carries: {work:?}, steps past {STEPS_PIN}"
    );
    assert_eq!(
        old,
        vec![Err(Exhausted)],
        "growing carries: the replaced scan stays within {bound}"
    );
}

/// Six-thousand reseals and edits of one value beside each other: an op on their merge holds the
/// edits alone, every reseal replaced, and its frontier's rows are read over the one word that
/// holds the reseals' rep: a word per op of it.
/// NEGATIVE CONTROL: reading the frontier's whole rows reads past that.
#[test]
fn a_frontier_of_reseals_and_edits_reads_one_word_per_op() {
    let mut rng = Rng(6_000);
    let (mut parents, lower) = fanned(1, 0);
    let root = lower[0];
    let mut ops = vec![put(&mut rng, root, 0)];
    let mut level = Vec::new();
    for i in 0..6_000 {
        let x = parents.len();
        parents.push(vec![root]);
        ops.push(put(&mut rng, x, usize::from(i % 2 == 1)));
        level.push(x);
    }
    while level.len() > 1 {
        let mut next = Vec::new();
        for chunk in level.chunks(64) {
            next.push(parents.len());
            parents.push(chunk.to_vec());
        }
        level = next;
    }
    let on = parents.len();
    parents.push(vec![level[0]]);
    ops.push(put(&mut rng, on, 2));
    let registers = vec![ops];
    let (got, _) = resolve(&parents, &registers);
    let reseals = (0..6_000).filter(|i| i % 2 == 0).count();
    assert_eq!(got[0].carries.iter().filter(|&&c| c).count(), reseals);
    assert_eq!(
        got[0].siblings,
        vec![6_001],
        "the last write replaces every other"
    );
    assert_eq!(disagreements(&parents, &registers), 0);

    // What the last write's writer held: its frontier is the 6,000 reseals and edits.
    let reach = indexed(&parents);
    let register = Register::classify_all(&[registers[0].as_slice()], &reach, |x| {
        parents[x].iter().copied()
    })
    .pop()
    .expect("one register");
    let frontier: Vec<usize> = (1..=6_000).collect();
    let edits: Vec<usize> = frontier.iter().copied().filter(|p| p % 2 == 0).collect();
    let bound = frontier.len() + 64;
    for mutant in [None, Some(Mutant::FullRows)] {
        let (held, work) = with_mutants(mutant, None, || {
            WORK.set(Work::NONE);
            let held = register.live(&frontier, &reach);
            (held, WORK.get())
        });
        assert_eq!(held, edits, "{mutant:?}: the edits replaced every reseal");
        match mutant {
            None => assert!(work.words <= bound, "{work:?}: words past {bound}"),
            Some(_) => assert!(work.words > bound, "whole rows go unnoticed: {work:?}"),
        }
    }
}
