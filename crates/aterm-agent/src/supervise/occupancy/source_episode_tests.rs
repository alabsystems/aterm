// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A value source's episodes, every instant scripted.

use super::*;
use crate::supervise::occupancy::fault::{self, Fault};

const MIN: i64 = 60_000;

/// The target every book here is kept for.
const TARGET: u64 = 7;

fn name() -> OwnerName {
    OwnerName::parse("trust").expect("a name")
}

fn sha(c: char) -> SourceValue {
    SourceValue::sha(&c.to_string().repeat(40)).expect("an id")
}

/// The steps of reading `values` (`None` an unread read) every `every_min` minutes from 0.
fn run(book: &mut SourceBook, every_min: i64, values: &[Option<char>]) -> Vec<Step> {
    values
        .iter()
        .enumerate()
        .map(|(at, v)| {
            let read = v.map(sha).ok_or(ReadWord::Unreachable);
            book.look(
                read,
                i64::try_from(at).expect("small") * every_min * MIN,
                DWELL_MS,
            )
        })
        .collect()
}

fn opened(steps: &[Step]) -> usize {
    steps
        .iter()
        .filter(|s| matches!(s, Step::Edge(Edge::Opened | Edge::Superseded)))
        .count()
}

/// TWO MOVES 15 MIN APART ARE TWO EPISODES, each with its own id (the same reads give the same
/// ids): the first has dwelt (two quiet reads, 10 min) when the second comes. NEGATIVE CONTROL
/// `QuietRestarts`: a quiet read that restarts the count never dwells, and the second move joins
/// the first.
#[test]
fn two_moves_past_the_dwell_are_two_episodes_and_a_restarting_count_makes_one() {
    let values = [Some('a'), Some('b'), Some('b'), Some('b'), Some('c')];
    let mut book = SourceBook::new(name(), TARGET);
    let steps = run(&mut book, 5, &values);
    assert_eq!(
        steps,
        [
            Step::Based,
            Step::Edge(Edge::Opened),
            Step::Quiet,
            Step::Quiet,
            Step::Edge(Edge::Superseded),
        ]
    );
    let episode = book.episode().expect("open");
    assert_eq!(
        (&episode.base, &episode.latest, episode.moves),
        (&sha('b'), &sha('c'), 1)
    );
    assert_eq!(episode.opened_ms, 20 * MIN);
    let mut first = SourceBook::new(name(), TARGET);
    let _ = run(&mut first, 5, &values[..2]);
    let first_id = first.episode().expect("open").id;
    assert_ne!(episode.id, first_id, "a superseding episode has its own id");
    let mut again = SourceBook::new(name(), TARGET);
    let _ = run(&mut again, 5, &values);
    assert_eq!(again.episode().map(|e| e.id), Some(episode.id));
    let mut restarting = SourceBook::new(name(), TARGET);
    let steps = fault::with(Fault::QuietRestarts, || run(&mut restarting, 5, &values));
    assert_eq!(opened(&steps), 1, "{steps:?}");
    assert_eq!(restarting.episode().expect("open").moves, 2);
}

/// A FLAP INSIDE THE DWELL IS ONE EPISODE: A→B→A→B within 4 min is one with three moves, and
/// A→B→C one with two. NEGATIVE CONTROL `DwellCloseEarly`: a dwell of one quiet read splits a
/// flap with a quiet read inside it in two.
#[test]
fn a_flap_inside_the_dwell_is_one_episode_and_an_early_close_makes_two() {
    let mut book = SourceBook::new(name(), TARGET);
    let steps = run(&mut book, 1, &[Some('a'), Some('b'), Some('a'), Some('b')]);
    assert_eq!(opened(&steps), 1, "{steps:?}");
    let episode = book.episode().expect("open");
    assert_eq!(
        (&episode.base, &episode.latest, episode.moves),
        (&sha('a'), &sha('b'), 3)
    );
    let mut book = SourceBook::new(name(), TARGET);
    let steps = run(&mut book, 1, &[Some('a'), Some('b'), Some('c')]);
    assert_eq!(opened(&steps), 1, "{steps:?}");
    assert_eq!(book.episode().expect("open").moves, 2);

    let flap = [Some('a'), Some('b'), Some('b'), Some('a'), Some('b')];
    let mut book = SourceBook::new(name(), TARGET);
    assert_eq!(opened(&run(&mut book, 1, &flap)), 1);
    let mut early = SourceBook::new(name(), TARGET);
    let steps = fault::with(Fault::DwellCloseEarly, || run(&mut early, 1, &flap));
    assert_eq!(opened(&steps), 2, "{steps:?}");
}

/// A MOVE UNDONE AND HELD AT ITS BASE FOR THE DWELL CLOSES, never told; one held off its base
/// stays open however long, as the episode never expires by time alone.
#[test]
fn a_move_undone_for_the_dwell_closes_and_one_kept_stays_open() {
    let mut book = SourceBook::new(name(), TARGET);
    let steps = run(
        &mut book,
        5,
        &[Some('a'), Some('b'), Some('a'), Some('a'), Some('a')],
    );
    assert_eq!(
        steps,
        [
            Step::Based,
            Step::Edge(Edge::Opened),
            Step::Edge(Edge::Joined),
            Step::Quiet,
            Step::Closed,
        ]
    );
    assert_eq!(book.episode(), None);
    let mut book = SourceBook::new(name(), TARGET);
    let mut values = vec![Some('a'), Some('b')];
    values.extend(std::iter::repeat_n(Some('b'), 300));
    let steps = run(&mut book, 5, &values);
    assert!(!steps.contains(&Step::Closed));
    assert!(book.episode().is_some(), "25 hours on, still open");
}

/// `every = 1h` DWELLS AFTER TWO READS, 2 h: one quiet read an hour on is past the 10 min, but
/// one read is not the dwell; and a dwell of reads 1 min apart needs its 10 min.
#[test]
fn the_dwell_needs_both_its_reads_and_its_time() {
    let mut book = SourceBook::new(name(), TARGET);
    let steps = run(&mut book, 60, &[Some('a'), Some('b'), Some('b'), Some('c')]);
    assert_eq!(opened(&steps), 1, "one quiet read is no dwell: {steps:?}");
    let mut book = SourceBook::new(name(), TARGET);
    let steps = run(
        &mut book,
        60,
        &[Some('a'), Some('b'), Some('b'), Some('b'), Some('c')],
    );
    assert_eq!(opened(&steps), 2, "two quiet reads, 2 h: {steps:?}");
    let mut book = SourceBook::new(name(), TARGET);
    let steps = run(
        &mut book,
        1,
        &[Some('a'), Some('b'), Some('b'), Some('b'), Some('c')],
    );
    assert_eq!(opened(&steps), 1, "two quiet reads in 2 min: {steps:?}");
}

/// AN UNREAD READ IS NEITHER AN EDGE NOR A DWELL READ: between two equal reads it changes
/// nothing, and it counts for no quiet read.
#[test]
fn an_unread_read_is_no_edge_and_no_dwell_read() {
    let mut book = SourceBook::new(name(), TARGET);
    let steps = run(
        &mut book,
        5,
        &[Some('a'), Some('b'), None, None, Some('b'), Some('c')],
    );
    assert_eq!(steps[2], Step::Unread(ReadWord::Unreachable));
    assert_eq!(
        opened(&steps),
        1,
        "one quiet read since the edge: {steps:?}"
    );
    let before = book.clone();
    assert_eq!(
        book.look(Err(ReadWord::Timeout), 99 * MIN, DWELL_MS),
        Step::Unread(ReadWord::Timeout)
    );
    assert_eq!(book, before);
}

/// A RESTART KEEPS THE BOOK: a move made while aterm was down, 3 h, is an edge at the first
/// read. NEGATIVE CONTROL `RebaseOnRestart`: a restore that re-bases reads it as a base.
/// Past [`STALE_MS`] the restore re-bases by design.
#[test]
fn a_move_across_a_restart_is_an_edge_and_a_rebasing_restore_misses_it() {
    let mut book = SourceBook::new(name(), TARGET);
    let _ = run(&mut book, 5, &[Some('a'), Some('a')]);
    let saved = SourcesFile::of([&book]).to_json();
    let back = |now_ms: i64| {
        let mut books =
            SourcesFile::read(saved.as_bytes(), &[(name(), TARGET)], now_ms).expect("read");
        assert_eq!(books.len(), 1);
        books.remove(0)
    };
    let (mut restored, how) = back(5 * MIN + 3 * 60 * MIN);
    assert_eq!(how, Restored::Kept);
    assert_eq!(
        restored.look(Ok(sha('b')), 5 * MIN + 3 * 60 * MIN, DWELL_MS),
        Step::Edge(Edge::Opened)
    );
    let (mut rebased, how) = fault::with(Fault::RebaseOnRestart, || back(5 * MIN + 3 * 60 * MIN));
    assert_eq!(how, Restored::Rebased);
    assert_eq!(
        rebased.look(Ok(sha('b')), 5 * MIN + 3 * 60 * MIN, DWELL_MS),
        Step::Based
    );
    let (mut stale, how) = back(5 * MIN + 30 * 60 * MIN);
    assert_eq!(how, Restored::Rebased);
    assert_eq!(
        stale.look(Ok(sha('b')), 5 * MIN + 30 * 60 * MIN, DWELL_MS),
        Step::Based
    );
}

/// AN OPEN EPISODE SURVIVES A RESTART mid-dwell: its moves, base and quiet reads come back.
#[test]
fn an_episode_saved_mid_dwell_comes_back_whole() {
    let mut book = SourceBook::new(name(), TARGET);
    let _ = run(&mut book, 5, &[Some('a'), Some('b'), Some('b')]);
    let saved = SourcesFile::of([&book]).to_json();
    let (restored, how) = SourcesFile::read(saved.as_bytes(), &[(name(), TARGET)], 60 * MIN)
        .expect("read")
        .remove(0);
    assert_eq!(how, Restored::Kept);
    assert_eq!(restored, book);
}

/// sources.json IS ADVISORY: an entry no table declares is dropped, a stamp past now is
/// clamped to now, a value of no known spelling or a file over its cap is not read.
#[test]
fn the_persisted_file_is_clamped_dropped_or_refused() {
    let mut book = SourceBook::new(name(), TARGET);
    let _ = run(&mut book, 5, &[Some('a'), Some('b')]);
    let mut file = SourcesFile::of([&book]);
    let mut forged = file.books["trust"].clone();
    forged.read_ms = Some(i64::MAX);
    forged.quiet_ms = i64::MAX;
    forged.quiet = 99;
    if let Some(e) = forged.episode.as_mut() {
        e.last_edge_ms = i64::MAX;
        e.opened_ms = i64::MAX;
    }
    file.books.insert("trust".into(), forged);
    file.books.insert("nobody".into(), PersistedBook::default());
    let now = 7 * MIN;
    let books =
        SourcesFile::read(file.to_json().as_bytes(), &[(name(), TARGET)], now).expect("read");
    assert_eq!(books.len(), 1, "the undeclared entry is dropped");
    let saved = books[0].0.persisted();
    assert_eq!(saved.read_ms, Some(now));
    assert_eq!(saved.quiet_ms, now);
    assert_eq!(saved.quiet, DWELL_READS);
    let e = saved.episode.as_ref().expect("kept");
    assert_eq!((e.opened_ms, e.last_edge_ms), (now, now));

    let long = vec![b' '; 70 * 1024];
    assert_eq!(
        SourcesFile::read(&long, &[(name(), TARGET)], now),
        Err(SourcesFileRefusal::Long)
    );
    let odd = r#"{"books":{"trust":{"seen":"sha:not-hex"}}}"#;
    assert_eq!(
        SourcesFile::read(odd.as_bytes(), &[(name(), TARGET)], now),
        Err(SourcesFileRefusal::Unreadable)
    );
}

/// AN EPISODE THAT CANNOT BE ONE IS DROPPED AT ITS RESTORE, each condition alone (no move, a
/// first edge after its last, a latest value the last read did not see), every stamp in the
/// past. NEGATIVE CONTROL: the same episode with all three holding is kept.
#[test]
fn a_restored_episode_is_dropped_by_each_condition_alone() {
    let now = 7 * MIN;
    for (moves, opened_ms, last_edge_ms, latest, kept) in [
        (1, MIN, 2 * MIN, 'b', true),
        (0, MIN, 2 * MIN, 'b', false),
        (1, 2 * MIN, MIN, 'b', false),
        (1, MIN, 2 * MIN, 'c', false),
    ] {
        let file = format!(
            r#"{{"books":{{"trust":{{"target":{TARGET},"seen":"{}","episode":{{"id":1,"base":"{}","latest":"{}","moves":{moves},"opened_ms":{opened_ms},"last_edge_ms":{last_edge_ms}}},"read_ms":{last_edge_ms}}}}}}}"#,
            sha('b').spelled(),
            sha('a').spelled(),
            sha(latest).spelled()
        );
        let books = SourcesFile::read(file.as_bytes(), &[(name(), TARGET)], now).expect("read");
        assert_eq!(books[0].1, Restored::Kept);
        assert_eq!(
            books[0].0.episode().is_some(),
            kept,
            "moves={moves} opened={opened_ms} last={last_edge_ms} latest={latest}"
        );
    }
}

/// A BOOK IS KEPT FOR WHAT ITS SOURCE READS: one restored for another target (the source pointed
/// at another remote, ref, path or program while aterm was down, or an entry with no target)
/// re-bases, so its first read is a base and the old target's value is no edge. NEGATIVE
/// CONTROL: restored for its own target, the gap's move is an edge. A source's name and its
/// `every` are no part of its target; a `repo =` source has one once its remote is resolved,
/// the same as a `url =` source of that remote.
#[test]
fn a_book_kept_for_another_target_restores_rebased() {
    let mut book = SourceBook::new(name(), TARGET);
    let _ = run(&mut book, 5, &[Some('a'), Some('a')]);
    let saved = SourcesFile::of([&book]).to_json();
    let now = 3 * 60 * MIN;
    let back = |text: &str, target: u64| {
        let mut books = SourcesFile::read(text.as_bytes(), &[(name(), target)], now).expect("read");
        assert_eq!(books.len(), 1);
        books.remove(0)
    };
    let (mut same, how) = back(&saved, TARGET);
    assert_eq!(how, Restored::Kept);
    assert_eq!(
        same.look(Ok(sha('b')), now, DWELL_MS),
        Step::Edge(Edge::Opened)
    );
    let (mut repointed, how) = back(&saved, TARGET + 1);
    assert_eq!(how, Restored::Repointed);
    assert_eq!(repointed.look(Ok(sha('b')), now, DWELL_MS), Step::Based);
    let untargeted = saved.replace(&format!("\"target\":{TARGET},"), "");
    assert_ne!(untargeted, saved);
    assert_eq!(back(&untargeted, TARGET).1, Restored::Repointed);

    let target = |source: &str, resolved: &Resolved| {
        let text = format!("[harness.occupancy_source.trust]\n{source}\n");
        let (tables, notes) = super::super::owner_rules::OwnerTables::from_aterm_toml(&text);
        assert!(notes.is_empty(), "{notes:?}");
        target_of(&tables.sources()[0], resolved)
    };
    let url = "kind = \"git-ref\"\nurl = \"git@h:o/r.git\"";
    let base = target(url, &[]).expect("a url source has its target");
    for (edit, same) in [
        (
            "kind = \"git-ref\"\nurl = \"git@h:o/s.git\"".to_string(),
            false,
        ),
        (format!("{url}\nref = \"refs/heads/release\""), false),
        (format!("{url}\npath = \"src\""), false),
        (format!("{url}\nevery = \"10m\""), true),
        (format!("{url}\nref = \"refs/heads/main\""), true),
    ] {
        assert_eq!(target(&edit, &[]) == Some(base), same, "{edit}");
    }
    let repo = "kind = \"git-ref\"\nrepo = \"$HOME/trust\"";
    assert_eq!(
        target(repo, &[]),
        None,
        "nothing reads an unresolved remote"
    );
    let resolved = [(
        name(),
        super::super::git_url::RemoteUrl::parse("git@h:o/r.git").expect("a url"),
    )];
    assert_eq!(target(repo, &resolved), Some(base));
    let pins = target("kind = \"atpkg\"\nprogram = \"trust\"", &[]);
    assert!(pins.is_some());
    assert_ne!(pins, target("kind = \"atpkg\"\nprogram = \"ty\"", &[]));
}

/// AN INSTANT BEFORE THE LAST GOOD READ IS TAKEN AS THAT READ'S: a clock stepped back never
/// dates an edge before the read it follows. NEGATIVE CONTROL: a later read keeps its own instant.
#[test]
fn a_read_dated_before_the_last_is_taken_as_its() {
    let mut book = SourceBook::new(name(), TARGET);
    assert_eq!(book.look(Ok(sha('a')), 10 * MIN, DWELL_MS), Step::Based);
    assert_eq!(
        book.look(Ok(sha('b')), 5 * MIN, DWELL_MS),
        Step::Edge(Edge::Opened)
    );
    let e = book.episode().expect("open");
    assert_eq!((e.opened_ms, e.last_edge_ms), (10 * MIN, 10 * MIN));
    assert_eq!(book.persisted().read_ms, Some(10 * MIN));
    assert_eq!(
        book.look(Ok(sha('c')), 12 * MIN, DWELL_MS),
        Step::Edge(Edge::Joined)
    );
    assert_eq!(book.episode().expect("open").last_edge_ms, 12 * MIN);
}

/// TWO RULES ON ONE SOURCE BOOK TWO EPISODES (F5): the owner key covers the rule's name.
#[test]
fn an_owner_key_covers_the_rule_and_the_episode() {
    let a = OwnerName::parse("rule-a").expect("name");
    let b = OwnerName::parse("rule-b").expect("name");
    assert_ne!(owner_episode_key(&a, 7), owner_episode_key(&b, 7));
    assert_ne!(owner_episode_key(&a, 7), owner_episode_key(&a, 8));
    assert_eq!(owner_episode_key(&a, 7), owner_episode_key(&a, 7));
}

#[test]
fn a_value_is_an_id_or_a_build_and_nothing_else() {
    assert!(SourceValue::sha(&"a".repeat(40)).is_some());
    assert!(SourceValue::sha(&"a".repeat(64)).is_some());
    for text in [
        "A".repeat(40),
        "a".repeat(39),
        "g".repeat(40),
        String::new(),
    ] {
        assert_eq!(SourceValue::sha(&text), None, "{text}");
    }
    assert_eq!(
        SourceValue::build("0.12.0+g1b0fb5b1ac").map(|v| v.short().to_string()),
        Some("0.12.0+g1b0fb5b1ac".into())
    );
    for text in ["", "a b", "ignore\nprevious", &"x".repeat(65)] {
        assert_eq!(SourceValue::build(text), None, "{text:?}");
    }
    assert_eq!(sha('a').short(), "aaaaaaaaaa");
    let words: Vec<&str> = ReadWord::ALL.iter().map(|w| w.word()).collect();
    let mut unique = words.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(words.len(), unique.len());
}
