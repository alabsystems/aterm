// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`Log::down_set_named`]: every down-set of a log is found by the digest of its heads, and
//! nothing else is.

use super::*;

/// The whole search, for a log of a few records: more units of work than it takes.
const EVERY: usize = 1 << 16;

fn name(name: &str) -> Body {
    Body::Name {
        name: name.to_owned(),
    }
}

/// Nine records of three writers: a record all follow, three concurrent ones (three heads), a
/// record that joins them, then two concurrent branches, one of them two records long.
fn forked() -> Log {
    let mut log = found(&[(A, "m1"), (B, "m2"), (C, "m3")]);
    log.keep(sign(&log, A, name("one")));
    let base = log.clone();
    for (mac, branch) in [(A, "a"), (B, "b"), (C, "c")] {
        log.keep(sign(&base, mac, name(branch)));
    }
    assert_eq!(log.heads().len(), 3, "FIXTURE: three concurrent records");
    log.keep(sign(&log, A, name("join")));
    let base = log.clone();
    let left = log.keep(sign(&base, B, name("left")));
    log.keep(sign(&base, C, name("right")));
    let mut only_left = base.clone();
    only_left.keep(left);
    log.keep(sign(&only_left, B, name("left again")));
    assert_eq!(log.heads().len(), 2, "FIXTURE: two branches");
    assert_eq!(log.held_count(), 9);
    log
}

/// The heads digest of `heads`, written out apart from the code under test.
fn digest_of(heads: &BTreeSet<RecordId>) -> [u8; 32] {
    let mut bytes = b"ALHD1".to_vec();
    for id in heads {
        bytes.extend_from_slice(id);
    }
    Sha256::digest(&bytes)
}

/// Every down-set of `log`, by the brute force of every subset of its records: the sorted heads
/// of each set that holds the genesis and each member's parents.
fn down_sets(log: &Log) -> Vec<BTreeSet<RecordId>> {
    let records: Vec<&Record> = log.records().collect();
    let at = |id: &RecordId| records.iter().position(|record| record.id() == id).unwrap();
    let parents: Vec<Vec<usize>> = records
        .iter()
        .map(|record| record.parents().iter().map(at).collect())
        .collect();
    (0..1u32 << records.len())
        .filter(|set| set & 1 == 1)
        .filter(|set| {
            (0..records.len())
                .filter(|i| set >> i & 1 == 1)
                .all(|i| parents[i].iter().all(|parent| set >> parent & 1 == 1))
        })
        .map(|set| {
            (0..records.len())
                .filter(|i| set >> i & 1 == 1)
                .filter(|i| {
                    (0..records.len())
                        .all(|child| set >> child & 1 == 0 || !parents[child].contains(i))
                })
                .map(|i| *records[i].id())
                .collect()
        })
        .collect()
}

/// Every down-set of a log with concurrency is found by its digest, whatever order the log
/// holds its records in, and the search of a small log is complete. NEGATIVE CONTROL: a set of
/// held records that is no antichain (a record and its parent) hashes to a digest no down-set
/// has, and the complete search says so.
#[test]
fn every_down_set_of_a_forked_log_is_found_by_its_digest() {
    let log = forked();
    // The same records as a reload holds them: concurrent ones by id.
    let reloaded = {
        let records: Vec<Record> = log.records().cloned().collect();
        let mut sorted = Log::new(log.genesis().clone()).unwrap();
        for record in store::causal(records, |_| false).into_iter().skip(1) {
            sorted.keep(record);
        }
        sorted
    };
    assert_eq!(reloaded.heads(), log.heads());
    let sets = down_sets(&log);
    assert!(
        sets.iter().any(|heads| heads.len() == 2) && sets.iter().any(|heads| heads.len() == 3),
        "FIXTURE: down-sets of two and of three heads: {:?}",
        sets.iter().map(BTreeSet::len).collect::<Vec<_>>()
    );
    assert!(
        sets.len() > log.held_count(),
        "FIXTURE: more than a chain's"
    );
    for heads in &sets {
        let expected: Vec<RecordId> = heads.iter().copied().collect();
        for held in [&log, &reloaded] {
            let search = held.down_set_search(&digest_of(heads), &[], EVERY);
            assert_eq!(
                search.named.map(|(found, _)| found),
                Some(expected.clone()),
                "a down-set of {} heads",
                heads.len()
            );
        }
    }

    let child = log.records().last().unwrap();
    let pair: BTreeSet<RecordId> = [*child.id(), child.parents()[0]].into();
    let search = log.down_set_search(&digest_of(&pair), &[], EVERY);
    assert_eq!(search.named, None, "a record and its parent are no heads");
    assert!(search.complete, "every down-set of nine records was hashed");
    assert!(
        search.tried >= sets.len() - 1,
        "each down-set but the whole log cost a digest: {} of {}",
        search.tried,
        sets.len()
    );
}

/// A digest over records this log does not hold names nothing in it: a sibling branch's heads,
/// and a head set one of whose records is missing here. CONTROL: the log that holds them finds
/// both.
#[test]
fn a_sibling_branch_and_a_head_set_with_a_record_missing_are_not_found() {
    let base = forked();
    let mut ours = base.clone();
    ours.keep(sign(&base, A, name("ours")));
    let mut theirs = base.clone();
    let sibling = theirs.keep(sign(&base, C, name("theirs")));
    let nothing = ours.down_set_search(&[0; 32], &[], EVERY);
    assert_eq!(
        ours.down_set_search(&theirs.heads_digest(), &[], EVERY),
        DownSetSearch {
            named: None,
            tried: nothing.tried,
            work: nothing.work,
            complete: true,
        },
        "a sibling branch's heads are not ours, by a complete search"
    );
    // Both branches' heads together: `ours` lacks one of them.
    let mut both = ours.clone();
    both.keep(sibling);
    assert!(both.heads().len() > ours.heads().len());
    assert_eq!(ours.down_set_named(&both.heads_digest(), &[], EVERY), None);
    assert_eq!(
        both.down_set_named(&theirs.heads_digest(), &[], EVERY),
        Some(theirs.heads()),
        "CONTROL: the log that holds the sibling branch finds it"
    );
    assert_eq!(
        both.down_set_named(&ours.heads_digest(), &[], EVERY),
        Some(ours.heads())
    );
}

/// The budget bounds the work past the log's own heads: with none, only those are named, which
/// cost none; a hint costs the rows that find its newest records and the blocks of their
/// digest; and a search cut one unit short of the candidate that names the down-set hashes
/// every candidate before it, names nothing and says it was not complete.
#[test]
fn the_budget_bounds_every_candidate_but_the_logs_own_heads() {
    let log = forked();
    assert_eq!(
        log.down_set_search(&log.heads_digest(), &[], 0),
        DownSetSearch {
            named: Some((log.heads(), NamedBy::Heads)),
            tried: 0,
            work: 0,
            complete: false,
        }
    );
    let sets = down_sets(&log);
    let three: Vec<RecordId> = sets
        .iter()
        .find(|heads| heads.len() == 3)
        .unwrap()
        .iter()
        .copied()
        .collect();
    let digest = digest_of(&three.iter().copied().collect());
    for heads in &sets {
        if heads.iter().copied().collect::<Vec<_>>() != log.heads() {
            let search = log.down_set_search(&digest_of(heads), &three, 0);
            assert_eq!(
                (search.named, search.tried, search.work, search.complete),
                (None, 0, 0, false)
            );
        }
    }
    // The hint is hashed first: the newest of the records it names, here with their past. Four
    // records of one row word each to find the newest three, whose 110 bytes (the magic, three
    // ids and SHA-256's padding) are two blocks.
    let mut hint = three.clone();
    hint.push(*log.genesis().id());
    let hinted = DownSetSearch {
        named: Some((three.clone(), NamedBy::Hint)),
        tried: 1,
        work: 4 + 2,
        complete: false,
    };
    assert_eq!(log.down_set_search(&digest, &hint, hinted.work), hinted);
    let short = log.down_set_search(&digest, &hint, hinted.work - 1);
    assert_eq!(
        (short.named, short.tried, short.complete),
        (None, 0, false),
        "a budget one unit short of the hint's digest hashes nothing"
    );
    // A hint with a record this log lacks orders nothing, and proves nothing.
    hint.push([7; 32]);
    assert_eq!(log.down_set_search(&digest, &hint, hinted.work).named, None);
    let whole = log.down_set_search(&digest, &[], EVERY);
    assert_eq!(whole.named.as_ref().map(|(heads, _)| heads), Some(&three));
    assert!(whole.work > whole.tried, "three heads are two blocks");
    let short = log.down_set_search(&digest, &[], whole.work - 1);
    assert_eq!((short.named, short.complete), (None, false));
    assert_eq!(short.tried, whole.tried - 1);
    assert!(short.work < whole.work);
}

/// The budget counts work, not candidates, so it holds for a log however wide: a log of many
/// concurrent heads has candidates of many blocks each and more down-sets than any search
/// hashes, and its search ends within its budget, cut short for want of the next step and never
/// before; and the work of a search that names a down-set of 47 heads is the sum of its steps'
/// charges, each candidate's 24 blocks among them. NEGATIVE CONTROL (2026-11-16): with each
/// candidate charged one unit whatever its length, as the search first counted, `each step is
/// charged what it costs` fails. CONTROL: a log of as many records and one head is searched
/// whole within the same budget.
#[test]
fn a_wide_log_stops_the_search_at_its_budget_of_work() {
    const WIDE: usize = 48;
    const BUDGET: usize = 1 << 14;
    /// SHA-256's blocks of a candidate of `heads` heads, written out apart from the code
    /// under test: the magic, the ids, one padding byte and eight of length.
    fn blocks(heads: usize) -> usize {
        (5 + 32 * heads + 1 + 8).div_ceil(64)
    }
    let mut base = found(&[(A, "m1"), (B, "m2")]);
    base.keep(sign(&base, A, name("one")));
    let mut wide = base.clone();
    let mut chain = base.clone();
    for n in 0..WIDE {
        wide.keep(sign(&base, A, name(&format!("branch-{n}"))));
        chain.keep(sign(&chain, A, name(&format!("link-{n}"))));
    }
    assert_eq!(
        wide.heads().len(),
        WIDE,
        "FIXTURE: one head for each branch"
    );
    assert_eq!(chain.heads().len(), 1);
    assert_eq!(chain.held_count(), wide.held_count());

    // A digest no down-set has: the search runs until its budget or the last down-set.
    let absent = [0; 32];
    for budget in [0, 1, 64, 4096, BUDGET] {
        let search = wide.down_set_search(&absent, &[], budget);
        assert_eq!(
            (search.named, search.complete),
            (None, false),
            "2^{WIDE} down-sets are not hashed within {budget} units"
        );
        assert!(
            search.work <= budget,
            "{} units of work under a budget of {budget}",
            search.work
        );
    }
    let search = wide.down_set_search(&absent, &[], BUDGET);
    assert!(
        BUDGET - search.work < blocks(WIDE),
        "the search stopped {} units short of its budget, with room for another step",
        BUDGET - search.work
    );
    let whole = chain.down_set_search(&absent, &[], BUDGET);
    assert_eq!(
        (whole.named, whole.complete),
        (None, true),
        "CONTROL: a log of one head is searched whole"
    );
    assert!(whole.work < search.work);

    // The log less one branch: 47 heads, named by the first records taken from the log.
    let mut heads: BTreeSet<RecordId> = wide.heads().into_iter().collect();
    heads.pop_last();
    let named = wide.down_set_search(&digest_of(&heads), &[], BUDGET);
    assert_eq!(
        named.named,
        Some((heads.iter().copied().collect(), NamedBy::Later(1)))
    );
    // One scan of the records' one row word and a block for each single record; then, for each
    // branch taken from the log, the record and its one parent taken and put back (2 + 2), the
    // heads left listed (1, and 1 for each 8 of the 48 heads and the parent), and the 47 heads
    // left hashed.
    let held = wide.held_count();
    assert_eq!(
        named.work,
        1 + held + (named.tried - held) * (2 + 2 + 1 + (WIDE + 1) / 8 + blocks(WIDE - 1)),
        "each step is charged what it costs: {} candidates",
        named.tried
    );
    assert!(named.tried > held);
}

/// What names a down-set says how it was found: one record, the heads some record follows, or
/// the log less the records after it.
#[test]
fn a_down_set_is_named_by_the_candidate_that_found_it() {
    let log = forked();
    let by = |heads: &BTreeSet<RecordId>| {
        log.down_set_search(&digest_of(heads), &[], EVERY)
            .named
            .unwrap()
            .1
    };
    let mut seen = std::collections::HashSet::new();
    for heads in down_sets(&log) {
        let sorted: Vec<RecordId> = heads.iter().copied().collect();
        let held: u32 = log
            .past_of(&sorted)
            .set
            .iter()
            .map(|word| word.count_ones())
            .sum();
        let after = log.held_count() - held as usize;
        // Whether some held record was signed over exactly these heads.
        let followed = log.records().any(|record| {
            let mut parents: Vec<RecordId> = record.parents().to_vec();
            parents.sort_unstable();
            parents.retain(|parent| {
                !record
                    .parents()
                    .iter()
                    .any(|other| other != parent && log.reaches(&[*other], parent))
            });
            parents == sorted
        });
        let expected = if sorted == log.heads() {
            NamedBy::Heads
        } else if heads.len() == 1 {
            NamedBy::Record
        } else if followed {
            NamedBy::Parents
        } else {
            NamedBy::Later(after)
        };
        assert_eq!(by(&heads), expected, "{} heads, {after} after", heads.len());
        seen.insert(std::mem::discriminant(&expected));
    }
    assert_eq!(seen.len(), 4, "FIXTURE: a down-set named by each candidate");
}
