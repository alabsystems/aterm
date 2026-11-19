// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Actors whose chains a rebuild writes straight into one run.
const REBUILT_ACTORS: u32 = 990;
/// Actors whose chains arrive through the delta, so they flush to level-0 runs and merge.
const INSERTED_ACTORS: u32 = 10;
/// Records each actor holds.
const SEQUENCES: u32 = 10_000;
/// Every key of the set.
const KEYS: u64 = (REBUILT_ACTORS + INSERTED_ACTORS) as u64 * SEQUENCES as u64;
const T0_MS: u64 = 1_800_000_000_000;

/// The chain index's key: (actor ordinal, sequence), big-endian so the order is numeric. Every
/// present key has an even sequence; the odd one beside it is absent.
fn chain_key(actor: u32, sequence: u32) -> Vec<u8> {
    let mut key = Vec::with_capacity(8);
    key.extend_from_slice(&actor.to_be_bytes());
    key.extend_from_slice(&sequence.to_be_bytes());
    key
}

fn record(actor: u32, at: u32) -> Vec<u8> {
    (u64::from(actor) * u64::from(SEQUENCES) + u64::from(at))
        .to_le_bytes()
        .to_vec()
}

fn chain(actor: u32, from: u32, to: u32) -> Vec<(Vec<u8>, Vec<u8>)> {
    (from..to)
        .map(|at| (chain_key(actor, 2 * at), record(actor, at)))
        .collect()
}

/// Looks `actor`'s record `at` up, and the absent key beside it, and holds each lookup to at most
/// one page of each run. Returns where the present key was found and the pages the absent lookup
/// read.
fn look(reader: &Reader, actor: u32, at: u32) -> (Source, u64) {
    let before = reader.pages_read();
    let hit = reader
        .get(&chain_key(actor, 2 * at))
        .unwrap()
        .expect("a present key is found");
    assert_eq!(hit.value, record(actor, at));
    let after = reader.pages_read();
    for ((run, was), (_, now)) in before.iter().zip(&after) {
        assert!(
            now - was <= 1,
            "a lookup of {actor}/{at} read {} pages of run {run}",
            now - was
        );
        if hit.source == Source::Run(*run) {
            assert_eq!(
                now - was,
                1,
                "the run that held {actor}/{at} read its one page"
            );
        }
    }
    assert_eq!(reader.get(&chain_key(actor, 2 * at + 1)).unwrap(), None);
    let absent = reader.pages_read();
    let mut read = 0;
    for ((run, was), (_, now)) in after.iter().zip(&absent) {
        assert!(
            now - was <= 1,
            "a lookup of the absent key beside {actor}/{at} read {} pages of run {run}",
            now - was
        );
        read += now - was;
    }
    (hit.source, read)
}

/// The run files' two promises (journal design §2.6, package 11). A lookup among 10,000,000 keys
/// reads at most one page of each run, the run that holds the key exactly one, and the filter
/// spares nearly every run that does not. And a reader that read mark 1 still reads every run that
/// mark named after 60 more marks at the design rate of one a second, though merges superseded
/// some of them: a superseded run stays for `reader_grace` (60 s) from the first mark that no
/// longer named it, not only for two marks. Once the grace has run out the runs are removed, and a
/// reader of mark 1 that had not yet opened them is told they are gone.
///
/// Negative control, run once: with `Runs::collect` removing a superseded run after two marks alone
/// (`grace_passed` taken as true), the test fails after the 60 marks at "run … is kept inside the
/// grace": the superseded runs the reader of mark 1 still names were already removed.
#[test]
fn a_lookup_among_10000000_keys_reads_one_page_a_run_and_a_reader_60_marks_old_still_reads_its_runs_inside_the_grace()
 {
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = root.path().join("chains");
    let key = StateKey::new(&[7; 32]);
    // A 256 KiB delta in place of 4 MiB keeps the levelled path's writes small in a debug test;
    // the rule is the same at either size.
    let policy = Policy {
        delta_bytes: 256 * 1024,
        grace_ms: READER_GRACE_MS,
    };
    let mut runs = Runs::open(&dir, key.clone(), Kind::Chains, policy, T0_MS).unwrap();

    let rebuilt = u64::from(REBUILT_ACTORS) * u64::from(SEQUENCES);
    let sorted = (0..REBUILT_ACTORS).flat_map(|actor| chain(actor, 0, SEQUENCES));
    runs.rebuild(3, 1, rebuilt, sorted).unwrap();
    let mut through = 1;
    for actor in REBUILT_ACTORS..REBUILT_ACTORS + INSERTED_ACTORS {
        for frame in 0..SEQUENCES / 1_000 {
            through += 1;
            runs.insert(through, &chain(actor, frame * 1_000, (frame + 1) * 1_000))
                .unwrap();
        }
    }
    runs.mark(T0_MS).unwrap();
    let in_runs: u64 = runs.live().iter().map(|run| run.entries).sum();
    assert_eq!(
        in_runs + runs.pending() as u64,
        KEYS,
        "the set holds 10,000,000 keys"
    );
    let levels: BTreeSet<u8> = runs.live().iter().map(|run| run.level).collect();
    assert!(
        runs.live().len() >= 4 && levels.len() >= 3 && runs.pending() > 0,
        "{:?}",
        runs.live()
    );

    // Two readers of mark 1: one looks up now, one not until the grace has run out.
    let reader = Reader::open(&dir, key.clone(), Kind::Chains).unwrap();
    let late = Reader::open(&dir, key.clone(), Kind::Chains).unwrap();
    let held = Reader::open(&dir, key.clone(), Kind::Chains).unwrap();
    assert_eq!(reader.mark(), 1);
    assert!(
        Reader::open(&dir, key.clone(), Kind::Ids).is_err(),
        "the chains' files do not open as the ids'"
    );

    let (mut sources, mut absent_pages, mut absent_lookups) = (BTreeSet::new(), 0, 0);
    let rebuilt_sample = (0..rebuilt).step_by(9_973);
    let inserted_sample = (rebuilt..KEYS).step_by(97);
    for at in rebuilt_sample.chain(inserted_sample) {
        let (actor, at) = (
            (at / u64::from(SEQUENCES)) as u32,
            (at % u64::from(SEQUENCES)) as u32,
        );
        let (source, pages) = look(&reader, actor, at);
        sources.insert(source);
        absent_pages += pages;
        absent_lookups += 1;
    }
    let every: BTreeSet<Source> = runs
        .live()
        .iter()
        .map(|run| Source::Run(run.sequence))
        .chain([Source::Delta])
        .collect();
    assert_eq!(sources, every, "the sample reached every run and the delta");
    let filtered = absent_lookups * runs.live().len() as u64;
    assert!(
        absent_pages * 100 <= filtered * 3,
        "the filters let {absent_pages} of {filtered} absent run lookups read a page"
    );
    println!(
        "{KEYS} keys in {} runs at levels {levels:?} and a delta of {}: {absent_lookups} present lookups read one page of the run that held each; absent lookups read {absent_pages} pages of {filtered} run lookups",
        runs.live().len(),
        runs.pending()
    );

    // Sixty marks, one a second, each after 2,000 new records: the delta flushes and level 0
    // merges, superseding runs and the delta that mark 1 named.
    for mark in 1..=60u32 {
        through += 2;
        let actor = REBUILT_ACTORS + INSERTED_ACTORS + mark;
        runs.insert(through - 1, &chain(actor, 0, 1_000)).unwrap();
        runs.insert(through, &chain(actor, 1_000, 2_000)).unwrap();
        runs.mark(T0_MS + u64::from(mark) * 1_000).unwrap();
    }
    assert_eq!(runs.mark_number() - held.mark(), 60);
    let live: BTreeSet<u64> = runs.live().iter().map(|run| run.sequence).collect();
    let superseded: Vec<u64> = held
        .manifest()
        .runs
        .iter()
        .map(|run| run.sequence)
        .filter(|sequence| !live.contains(sequence))
        .collect();
    assert!(
        !superseded.is_empty(),
        "merges superseded runs mark 1 named"
    );
    for sequence in &superseded {
        assert!(
            dir.join(run_name(*sequence)).exists(),
            "run {sequence} is kept inside the grace"
        );
    }
    let mut found_in = BTreeMap::new();
    for at in (rebuilt..KEYS).step_by(97) {
        let (actor, at) = (
            (at / u64::from(SEQUENCES)) as u32,
            (at % u64::from(SEQUENCES)) as u32,
        );
        found_in
            .entry(look(&held, actor, at).0)
            .or_insert((actor, at));
    }
    for sequence in &superseded {
        assert!(
            found_in.contains_key(&Source::Run(*sequence)),
            "the reader 60 marks old read superseded run {sequence}"
        );
    }
    println!(
        "a reader 60 marks old read {} superseded runs of its mark inside the grace",
        superseded.len()
    );

    // Past the grace the superseded runs go, and a reader of mark 1 that had not opened them is
    // told so; the newest mark still answers.
    runs.mark(T0_MS + 61_000 + READER_GRACE_MS).unwrap();
    for sequence in &superseded {
        assert!(
            !dir.join(run_name(*sequence)).exists(),
            "run {sequence} is removed after the grace"
        );
    }
    let (actor, at) = found_in[&Source::Run(superseded[0])];
    let gone = late
        .get(&chain_key(actor, 2 * at))
        .expect_err("a reader of mark 1 past the grace finds a superseded run gone");
    assert!(gone.0.contains("is gone"), "{}", gone.0);
    let newest = Reader::open(&dir, key, Kind::Chains).unwrap();
    assert_eq!(
        newest
            .get(&chain_key(actor, 2 * at))
            .unwrap()
            .map(|hit| hit.value),
        Some(record(actor, at))
    );
}
