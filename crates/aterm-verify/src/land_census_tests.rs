// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The census over a fixture store, and main's walk.

use super::*;
use crate::land_record::tests::{NOW, signer};
use crate::land_run::tests::{record, run};
use crate::land_run::{Cause, Landing, Run};

/// The window's start: a day before [`NOW`].
const SINCE: u64 = NOW - 86_400;

/// A commit id of `c`.
fn id(c: char) -> String {
    std::iter::repeat_n(c, 40).collect()
}

/// A run's id of `c`.
fn run_id(c: char) -> String {
    std::iter::repeat_n(c, 32).collect()
}

/// A planted landing: its kind, its commit ([`id`]) and whether it resumed a kept judgment.
type Landed = (&'static str, char, bool);

/// The steps of one planted run, each a kind, a result and a cause word.
struct Planted {
    id: char,
    host: &'static str,
    started: u64,
    steps: Vec<(Kind, &'static str, Option<&'static str>)>,
    /// The end: exit, signal and landing; `None`: no end record.
    end: Option<(i32, Option<i32>, Option<Landed>)>,
    /// The run's seconds to its end.
    secs: u64,
}

/// `planted`'s records, signed, read as the journal would hold them.
fn entries(planted: &[Planted]) -> Vec<Entry> {
    let me = signer(3);
    let mut entries = Vec::new();
    for run_of in planted {
        let ran = Run {
            id: run_id(run_of.id),
            started: run_of.started,
            host: run_of.host.to_owned(),
            ..run()
        };
        let mut records: Vec<Record> = run_of
            .steps
            .iter()
            .map(|(kind, result, cause)| Record {
                result: (*result).to_owned(),
                cause: cause.map(|word| Cause::new(word, "", "")),
                run: ran.clone(),
                ..record(*kind, 0)
            })
            .collect();
        if let Some((exit, signal, landing)) = run_of.end {
            records.push(Record {
                result: if landing.is_some() {
                    "landed"
                } else {
                    "not-landed"
                }
                .to_owned(),
                exit: Some(exit),
                signal,
                secs: run_of.secs,
                landing: landing.map(|(kind, commit, resumed)| Landing {
                    commit: id(commit),
                    onto: id('0'),
                    kind: kind.to_owned(),
                    resumed,
                }),
                run: ran.clone(),
                ..record(Kind::End, 0)
            });
        }
        for (seq, made) in records.into_iter().enumerate() {
            let made = Record {
                seq: u32::try_from(seq).unwrap(),
                ..made
            };
            let signed = made.sign(Some(&me)).expect("it signs");
            entries.push(Entry::read(&signed.path, &signed.bytes, Origin::Journal));
        }
    }
    entries
}

/// THE CENSUS PRINTS WHAT THE STORE HOLDS: over planted runs it counts each run once (the
/// same record spooled and in the journal read once), and prints r over the first judgments
/// that judged (one that did not run is counted apart), the share of runs that lost a race (an
/// abort, or a stop at the push bound), the runs that ended without landing by exit, signal or a
/// missing end, p50 and p95 minutes to main to a tenth (a landing of 1230 s is 20.5 minutes),
/// landings by kind, re-judges and rounds by cause with their results, composes and aborts, the
/// unreadable record, the one movement of main that holds no receipt, and the one receipted
/// landing no end record names. A run that started before the window is not counted, though its
/// landing still names its movement, and its start is the earliest a record was read from: a
/// receipted landing made before it is not asked for a record. NEGATIVE CONTROLS: the same
/// store from a window that starts after every run counts none of them and no first judgment,
/// while the movements their landings name stay recorded; and with no entry read no landing is
/// counted as missing its record. Mutants run, each of which made this test fail: a stop at
/// the push bound not counted as a lost race (2/9); r over every first judgment (1/6); minutes
/// taken as whole minutes before the percentile (p50 20); a receipted landing asked for its
/// record whenever it was made (2 of 6, the one from before the earliest record among them).
#[test]
fn the_census_prints_the_planted_values() {
    use Kind::{Abort, Compose, Judgment, Round};
    let at = |minutes: u64| SINCE + minutes * 60;
    let planted = [
        // A: a red first judgment.
        Planted {
            id: 'a',
            host: "mac",
            started: at(1),
            steps: vec![(Judgment, "fail", Some("first"))],
            end: Some((1, None, None)),
            secs: 600,
        },
        // B: a pass composed onto a moved main, landed in 20.5 minutes.
        Planted {
            id: 'b',
            host: "mac",
            started: at(2),
            steps: vec![
                (Judgment, "pass", Some("first")),
                (Compose, "composed", Some("disjoint")),
            ],
            end: Some((0, None, Some(("composed", '1', false)))),
            secs: 1_230,
        },
        // C: a first judgment cut, its re-judge passed, landed in 50 minutes.
        Planted {
            id: 'c',
            host: "mac",
            started: at(3),
            steps: vec![(Abort, "cut", Some("cut")), (Judgment, "pass", Some("cut"))],
            end: Some((0, None, Some(("exact", '2', false)))),
            secs: 3_000,
        },
        // D: a pass doomed at the push, its re-judge red.
        Planted {
            id: 'd',
            host: "mac",
            started: at(4),
            steps: vec![
                (Judgment, "pass", Some("first")),
                (Abort, "doomed", Some("doomed")),
                (Judgment, "fail", Some("doomed")),
            ],
            end: Some((1, None, None)),
            secs: 2_000,
        },
        // E: interrupted before it judged anything.
        Planted {
            id: 'e',
            host: "mac",
            started: at(5),
            steps: vec![],
            end: Some((3, Some(15), None)),
            secs: 30,
        },
        // F: a pass and no end record.
        Planted {
            id: 'f',
            host: "mac",
            started: at(6),
            steps: vec![(Judgment, "pass", Some("first"))],
            end: None,
            secs: 0,
        },
        // G: a round of a merge of main into a kept judgment, landed in 10 minutes.
        Planted {
            id: '7',
            host: "studio",
            started: at(7),
            steps: vec![(Round, "pass", Some("kept-merge"))],
            end: Some((0, None, Some(("rechecked", '3', true)))),
            secs: 600,
        },
        // H: before the window; its landing names a movement in it.
        Planted {
            id: '8',
            host: "mac",
            started: SINCE - 60,
            steps: vec![(Judgment, "pass", Some("first"))],
            end: Some((0, None, Some(("exact", '4', false)))),
            secs: 600,
        },
        // J: a first judgment that did not run.
        Planted {
            id: '6',
            host: "mac",
            started: at(8),
            steps: vec![(Judgment, "not-run", Some("first"))],
            end: Some((3, None, None)),
            secs: 120,
        },
        // I: stopped at the push bound while main kept moving.
        Planted {
            id: '9',
            host: "mac",
            started: at(9),
            steps: vec![(Judgment, "pass", Some("first"))],
            end: Some((crate::land::STOPPED, None, None)),
            secs: 900,
        },
    ];
    let mut store = entries(&planted);
    // The first record again, as the spool holds it until an append does: read once.
    store.push(Entry {
        origin: Origin::Spool,
        ..store[0].clone()
    });
    store.push(Entry::read(
        &format!("land/runs/{}/{}.0.end.toml", NOW / 86_400, run_id('5')),
        b"not a record",
        Origin::Journal,
    ));
    // Four landings the records name, one receipted landing no record names, a tip with no
    // receipt, and a receipted landing from before the earliest record read (run H's start).
    let moved = |commit: char, at: u64, landing: bool| Listed {
        commit: id(commit),
        parents: Vec::new(),
        at,
        landing,
    };
    let movements = [
        moved('6', at(12), false),
        moved('5', at(11), true),
        moved('3', at(10), true),
        moved('2', at(9), true),
        moved('1', at(8), true),
        moved('4', SINCE, true),
        moved('0', SINCE - 61, true),
    ];
    let lines = census(&store, &movements, SINCE);
    let expected = [
        format!(
            "verify land census since {} ({SINCE}): 9 runs from 2 hosts; 1 records do not read",
            aterm_types::rfc3339::format_rfc3339(SINCE)
        ),
        "  r (first judgments red): 1/5 = 20.0%; 1 did not run".to_owned(),
        "  runs that lost a race: 3/9 = 33.3%".to_owned(),
        "  runs that ended without landing: 6 (exit 1: 2, exit 3: 1, exit 75: 1, no end record: \
         1, signal 15: 1)"
            .to_owned(),
        "  minutes to main: p50 20.5, p95 50.0 (over 3 landed runs)".to_owned(),
        "  landings by kind: composed 1, exact 1, rechecked 1 (1 resumed a kept judgment)"
            .to_owned(),
        "  re-judges by cause: cut 1 (pass 1); doomed 1 (fail 1)".to_owned(),
        "  rounds by cause: kept-merge 1 (pass 1)".to_owned(),
        "  composes: 1; aborts: cut 1, doomed 1".to_owned(),
        format!("  main tips with no receipt: 1 of 7 ({})", id('6')),
        format!(
            "  receipted landings with no run record: 1 of 5 since {} (the earliest run record \
             read) ({})",
            aterm_types::rfc3339::format_rfc3339(SINCE - 60),
            id('5')
        ),
    ];
    assert_eq!(lines, expected);
    assert_eq!(
        census(&[], &movements, SINCE)[expected.len() - 2..],
        [
            format!("  main tips with no receipt: 1 of 7 ({})", id('6')),
            "  receipted landings with no run record: none counted (no run record was read)"
                .to_owned(),
        ]
    );

    let later = census(&store, &movements, NOW + 86_400);
    assert!(later[0].contains(": 0 runs from 0 hosts;"), "{later:?}");
    assert_eq!(later[1], "  r (first judgments red): 0/0; 0 did not run");
    assert_eq!(
        later[later.len() - 2..],
        expected[expected.len() - 2..],
        "every landing record names its movement, whatever window counts its run"
    );
}

/// MAIN'S MOVEMENTS ARE WHAT ITS LANDINGS PROVE: the tip, each landing commit and each landing's
/// second parent (the main its push saw), in the log's order, from the window on. A commit that
/// is not a landing has no parent followed: here `m` is a person's merge of their branch `f`
/// pushed as it stands, so `f` (its first parent) was never main, and `r`, a direct push the
/// landing `2` was pushed onto, is main without its parent `q`, which no landing proves. `git
/// log`'s lines read as commits, a landing told by its receipt's trailer. NEGATIVE CONTROL: the
/// same log with no landing in it proves only the tip. Mutant run, which made this test fail:
/// the earlier walk, stepping a landing to its second parent and any other commit to its first
/// (it counted `f` and `q` as main).
#[test]
fn mains_movements_are_the_tip_its_landings_and_the_mains_they_were_pushed_onto() {
    let line = |commit: char, parents: &[char], at: u64, trailers: &str| {
        let parents: Vec<String> = parents.iter().map(|c| id(*c)).collect();
        format!(
            "{} {}\u{1f}{at}\u{1f}{trailers}\u{1e}\n",
            id(commit),
            parents.join(" ")
        )
    };
    let log = [
        line('3', &['c', 'm'], SINCE + 60, "Aterm-Checked: x\n"),
        line('c', &['2'], SINCE + 55, ""),
        line('m', &['f', '2'], SINCE + 50, ""),
        line('f', &['2'], SINCE + 45, ""),
        line(
            '2',
            &['b', 'r'],
            SINCE + 40,
            "Aterm-Rechecked: y\nAterm-Checked: x\n",
        ),
        line('b', &['a'], SINCE + 35, ""),
        line('r', &['q'], SINCE + 30, "Signed-off-by: a person\n"),
        line('q', &['1'], SINCE + 25, ""),
        line('1', &['a', '0'], SINCE + 20, "Aterm-Checked: z\n"),
        line('a', &['0'], SINCE + 10, ""),
        line('0', &[], SINCE - 10, ""),
    ]
    .concat();
    let commits = listed(&log);
    assert_eq!(commits.len(), 11);
    assert!(commits[0].landing && commits[4].landing && !commits[6].landing);
    let moved = |listed: &[Listed]| -> Vec<(String, bool)> {
        mainline(&id('3'), listed, SINCE)
            .into_iter()
            .map(|commit| (commit.commit, commit.landing))
            .collect()
    };
    assert_eq!(
        moved(&commits),
        [
            (id('3'), true),
            (id('m'), false),
            (id('2'), true),
            (id('r'), false),
            (id('1'), true)
        ]
    );
    let unlanded: Vec<Listed> = commits
        .iter()
        .map(|c| Listed {
            landing: false,
            ..c.clone()
        })
        .collect();
    assert_eq!(moved(&unlanded), [(id('3'), false)]);
}

/// THE CENSUS COUNTS A SPOOLED RUN RECORD THAT DOES NOT READ: a spool file named as a run record
/// that could not be read is an unreadable entry at its journal path, beside the spooled run
/// records that read; a verdict record's file is no run record. NEGATIVE CONTROL, run once as a
/// mutant that made this test fail: [`spooled`] passing over the spool's unreadable files (the
/// census said 0 records do not read).
#[test]
fn a_spooled_run_record_that_does_not_read_is_counted_as_unreadable() {
    let me = signer(3);
    let end = Record {
        result: "not-landed".to_owned(),
        exit: Some(1),
        run: Run {
            id: run_id('a'),
            started: SINCE + 60,
            ..run()
        },
        ..record(Kind::End, 0)
    }
    .sign(Some(&me))
    .expect("it signs");
    let unread = format!("land/runs/{}/{}.0.end.toml", NOW / 86_400, run_id('b'));
    let file = |path: &str| {
        std::path::PathBuf::from("/spool").join(crate::land_spool::file_of(path).unwrap())
    };
    let verdict = crate::land_record::tests::record(
        crate::checks::Check::Fmt,
        crate::land_record::Word::Ok,
        crate::land_record::tests::T,
        crate::land_record::tests::BASE,
        NOW,
    )
    .sign(Some(&me))
    .unwrap();
    let pending = crate::land_spool::Pending {
        records: vec![end.clone(), verdict.clone()],
        unreadable: vec![
            (file(&unread), "file exceeds 65536 bytes".to_owned()),
            (file(&verdict.path), "not a regular file".to_owned()),
        ],
    };
    let entries = spooled(&pending);
    let paths: Vec<(&str, bool)> = entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry.body.is_ok()))
        .collect();
    assert_eq!(paths, [(end.path.as_str(), true), (unread.as_str(), false)]);
    let lines = census(&entries, &[], SINCE);
    assert!(
        lines[0].ends_with(": 1 runs from 1 hosts; 1 records do not read"),
        "{lines:?}"
    );
}

/// `--since` takes Unix seconds, days or hours before now, or a UTC day; nothing else.
/// NEGATIVE CONTROL, run as a mutant that made this test fail: `<n>d` read as hours.
#[test]
fn since_names_unix_seconds_days_hours_or_a_utc_day() {
    assert_eq!(since_of("1700000000", NOW), Some(1_700_000_000));
    assert_eq!(since_of("2d", NOW), Some(NOW - 2 * 86_400));
    assert_eq!(since_of("5h", NOW), Some(NOW - 5 * 3_600));
    assert_eq!(since_of("1970-01-02", NOW), Some(86_400));
    for word in ["", "d", "2w", "-1d", "yesterday", "1970-13-01"] {
        assert_eq!(since_of(word, NOW), None, "{word:?}");
    }
}
