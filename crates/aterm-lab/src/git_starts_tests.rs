// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for [`super`]: the counts over a fake [`Readers`], each beside the case that must count
//! otherwise.

use super::*;

/// A fake machine: the listing and the survey reads of the current look.
#[derive(Default)]
struct Fake {
    look: Look,
}

/// What one look reads: this uid's processes, each with what its read gives (`None` for the
/// whole listing: it failed), and the survey reads that finished since the last look.
#[derive(Clone, Default)]
struct Look {
    pids: Option<Vec<(u32, Option<Seen>)>>,
    surveys: Vec<(u64, Option<Vec<Row>>)>,
}

impl Readers for Fake {
    fn pids(&mut self) -> Option<Vec<u32>> {
        let pids = self.look.pids.as_ref()?;
        Some(pids.iter().map(|(pid, _)| *pid).collect())
    }

    fn process(&mut self, pid: u32) -> Option<Seen> {
        let pids = self.look.pids.as_ref()?;
        pids.iter()
            .find(|(p, _)| *p == pid)
            .and_then(|(_, seen)| *seen)
    }

    fn survey(&mut self) -> Option<(u64, Option<Vec<Row>>)> {
        (!self.look.surveys.is_empty()).then(|| self.look.surveys.remove(0))
    }
}

/// The tally of `looks`, 100 ms apart.
fn run(looks: impl IntoIterator<Item = Look>) -> Tally {
    let mut tally = Tally::default();
    let mut fake = Fake::default();
    for (at, look) in (0u64..).step_by(100).zip(looks) {
        fake.look = look;
        tally.look(at, &mut fake);
    }
    tally
}

fn listing(pids: &[(u32, Seen)]) -> Look {
    Look {
        pids: Some(pids.iter().map(|&(pid, seen)| (pid, Some(seen))).collect()),
        surveys: Vec::new(),
    }
}

const fn git(birth: u64) -> Seen {
    Seen { birth, git: true }
}

const fn other(birth: u64) -> Seen {
    Seen { birth, git: false }
}

fn row(sid: &str, agent: bool, word: &str) -> Row {
    Row {
        sid: sid.to_owned(),
        agent,
        word: word.to_owned(),
    }
}

/// A survey read finished at `at_ms` showing `rows`, as (sid, agent, word).
fn read(at_ms: u64, rows: &[(&str, bool, &str)]) -> (u64, Option<Vec<Row>>) {
    let rows = rows
        .iter()
        .map(|(sid, agent, word)| row(sid, *agent, word))
        .collect();
    (at_ms, Some(rows))
}

/// A look with an empty listing and the survey reads `reads`.
fn surveyed(reads: impl IntoIterator<Item = (u64, Option<Vec<Row>>)>) -> Look {
    Look {
        pids: Some(Vec::new()),
        surveys: reads.into_iter().collect(),
    }
}

#[test]
fn a_git_seen_in_many_samples_counts_once() {
    let shell = (10, other(1));
    let status = (500, git(2));
    let mut looks = vec![listing(&[shell])];
    looks.extend((0..5).map(|_| listing(&[shell, status])));
    // A listing that lacks it, then the same process again.
    looks.push(listing(&[shell]));
    looks.push(listing(&[shell, status]));
    assert_eq!(
        run(looks.clone()).gits.len(),
        1,
        "one git process counts once, however many listings show it"
    );
    let running = run([listing(&[status]), listing(&[status])]);
    assert_eq!(
        running.gits.len(),
        0,
        "a git the first listing shows started before the run"
    );
    looks.push(listing(&[shell]));
    looks.push(listing(&[shell, (500, git(3))]));
    assert_eq!(
        run(looks).gits.len(),
        2,
        "the same pid with a new birth is a second git start"
    );
}

#[test]
fn a_process_that_read_as_none_is_read_again() {
    let shell = (10, Some(other(1)));
    // Listed while it starts: its read gives nothing yet.
    let creating = Look {
        pids: Some(vec![shell, (500, None)]),
        surveys: Vec::new(),
    };
    let tally = run([
        listing(&[(10, other(1))]),
        creating,
        listing(&[(10, other(1)), (500, git(2))]),
        listing(&[(10, other(1)), (500, git(2))]),
    ]);
    assert_eq!(
        tally.gits.len(),
        1,
        "a pid whose read was none is read at the next listing, and its git counts"
    );
}

#[test]
fn process_starts_count_from_the_first_process_born_during_the_run() {
    // Before the run, this uid's newest-born process got pid 1_000; other uids' processes have
    // moved the counter on since.
    let old = (1_000, other(10));
    let tally = run([
        listing(&[old]),
        listing(&[old, (50_001, other(20))]),
        listing(&[old, (50_001, other(20)), (50_003, other(21))]),
    ]);
    assert_eq!(
        tally.starts, 2,
        "the pids handed out before the first process born during the run are not the run's"
    );
    assert_eq!(tally.counted, Some((100, 200)));
}

#[test]
fn process_starts_count_through_the_pid_wrap() {
    // Old processes sit anywhere in the counter's cycle: one ahead of the newest pid, one behind.
    let (ahead, behind) = ((40_000, other(1)), (60_000, other(2)));
    let newest = (99_990, other(10));
    let tally = run([
        listing(&[ahead, behind, newest]),
        // The first process born during the run.
        listing(&[ahead, behind, newest, (99_993, other(11))]),
        listing(&[ahead, behind, newest, (99_995, other(12))]),
        Look::default(),
        // The newest process ended: no step back.
        listing(&[ahead, behind, newest]),
        // One born in the same microsecond as the newest moves nothing on; the next later-born
        // process's advance covers it.
        listing(&[ahead, behind, newest, (99_996, other(12))]),
        // Two new processes either side of the wrap: the later-born is the newest, though its pid
        // is less. The counter hands out 99_996 to 99_998, then 100 to 105.
        listing(&[
            ahead,
            behind,
            newest,
            (99_996, other(12)),
            (99_997, other(20)),
            (105, other(21)),
        ]),
    ]);
    assert_eq!(
        tally.starts,
        2 + 9,
        "the newest-born process's pid advance, folded over the wrap"
    );
    assert_eq!((tally.listings, tally.failed), (6, 1));
}

#[test]
fn a_turn_end_is_a_row_in_a_turn_coming_to_rest() {
    let reads: [&[(&str, bool, &str)]; 6] = [
        &[
            ("s-a", true, "busy"),
            ("s-b", true, "busy"),
            ("s-c", true, "idle"),
            ("s-e", true, "busy"),
            ("s-sh", false, "-"),
        ],
        &[
            ("s-a", true, "prompt"),
            ("s-b", true, "busy"),
            ("s-c", true, "idle"),
            ("s-e", true, "busy"),
        ],
        &[
            ("s-a", true, "idle"),
            ("s-b", true, "busy"),
            ("s-c", true, "idle"),
            ("s-e", true, "busy"),
        ],
        &[
            ("s-a", true, "busy"),
            ("s-b", true, "busy"),
            ("s-c", true, "idle"),
            ("s-d", true, "busy"),
            ("s-e", true, "wall:usage-session"),
        ],
        &[
            ("s-a", true, "survey"),
            ("s-b", true, "-"),
            ("s-c", true, "idle"),
            ("s-d", true, "busy"),
            ("s-e", true, "wall:usage-session"),
        ],
        &[
            ("s-a", true, "prompt"),
            ("s-b", true, "busy"),
            ("s-c", true, "idle"),
            ("s-d", true, "question"),
            ("s-e", true, "idle"),
        ],
    ];
    let tally = run((0u64..)
        .step_by(5_000)
        .zip(reads)
        .map(|(at, rows)| surveyed([read(at, rows)])));
    let turn_ends = |sid: &str| tally.agents[sid].turn_ends.clone();
    assert_eq!(
        turn_ends("s-b"),
        Vec::<u64>::new(),
        "busy to busy, or to no agent word, is no turn end"
    );
    assert_eq!(
        turn_ends("s-c"),
        Vec::<u64>::new(),
        "a row that appears already resting counts none"
    );
    assert_eq!(
        turn_ends("s-a"),
        [10_000, 20_000],
        "an approval box is inside the turn: the rest after it ends the turn, as the feedback card does"
    );
    assert_eq!(
        turn_ends("s-e"),
        [15_000],
        "a wall ends the turn; the rest after it ends none"
    );
    assert_eq!(
        turn_ends("s-d"),
        [25_000],
        "a session started during the run, its turn ending on a question"
    );
    assert!(tally.agents["s-d"].new && !tally.agents["s-a"].new);
    assert_eq!((tally.sessions.len(), tally.agents.len()), (6, 5));
    assert_eq!(
        (tally.shown, tally.busy),
        (27, 12),
        "agent rows shown, and busy"
    );
}

#[test]
fn a_survey_read_an_aterm_did_not_answer_folds_nothing() {
    let tally = run([
        surveyed([(0, None)]),
        surveyed([read(1_000, &[("s-a", true, "busy")])]),
        surveyed([(2_000, None)]),
        surveyed([read(3_000, &[("s-a", true, "idle"), ("s-b", true, "busy")])]),
    ]);
    assert_eq!((tally.surveys, tally.unanswered), (2, 2));
    assert!(
        !tally.agents["s-a"].new,
        "a session the first answered read shows was running before the run"
    );
    assert!(tally.agents["s-b"].new);
    assert_eq!(tally.agents["s-a"].turn_ends, [3_000]);
}

#[test]
fn every_survey_read_finished_between_two_looks_is_folded() {
    // The listing loop fell behind: two reads finished before its next look, the turn end
    // between them.
    let tally = run([
        surveyed([read(0, &[("s-a", true, "busy")])]),
        surveyed([
            read(1_000, &[("s-a", true, "idle")]),
            read(2_000, &[("s-a", true, "busy")]),
        ]),
    ]);
    assert_eq!(
        tally.agents["s-a"].turn_ends,
        [1_000],
        "the rest seen only by the earlier read is a turn end"
    );
    assert_eq!(tally.surveys, 3, "each finished read is folded once");
}

#[test]
fn a_survey_pass_counts_only_where_every_listening_aterm_answered() {
    use std::io::{Error, ErrorKind};
    let instances = || {
        Ok::<_, ()>(vec![
            (1, "a".to_owned()),
            (2, "b".to_owned()),
            (3, "gone".to_owned()),
        ])
    };
    let answer = |times_out: &'static str| {
        move |sock: &str| match sock {
            "gone" => Err(Error::from(ErrorKind::ConnectionRefused)),
            s if s == times_out => Err(Error::from(ErrorKind::TimedOut)),
            s => Ok(vec![row(&format!("s-{s}"), true, "busy")]),
        }
    };
    assert_eq!(
        survey_pass(instances(), answer("b")),
        None,
        "an aterm that accepts and does not answer leaves the whole read unanswered"
    );
    assert_eq!(
        survey_pass(instances(), answer("-")),
        Some(vec![row("s-a", true, "busy"), row("s-b", true, "busy")]),
        "a socket nothing accepts on is an ended aterm's"
    );
    assert_eq!(
        survey_pass(Err::<Vec<(u32, String)>, _>(()), answer("-")),
        None
    );
}

#[test]
fn the_projection_caps_each_session_at_the_floor() {
    let every_5_s = Session {
        turn_ends: (0..60_000).step_by(5_000).collect(),
        last_ms: 55_000,
        ..Session::default()
    };
    assert_eq!(every_5_s.turn_ends.len(), 12);
    assert_eq!(
        floor_reads(&every_5_s, FLOOR),
        3,
        "a turn end every 5 s is read at 0, 20 and 40 s: 3 reads a minute, not 12"
    );
    let every_30_s = Session {
        turn_ends: vec![0, 30_000],
        ..Session::default()
    };
    assert_eq!(
        floor_reads(&every_30_s, FLOOR),
        2,
        "slower than the floor, each turn end reads"
    );
    let started = Session {
        new: true,
        turn_ends: vec![5_000, 20_000, 25_000],
        ..Session::default()
    };
    assert_eq!(
        floor_reads(&started, FLOOR),
        2,
        "its first look reads, then its turn end at 20 s"
    );
    let mut tally = run((0..=600).map(|_| listing(&[])));
    tally.agents.insert("s-a".into(), every_5_s);
    let summary = tally.summary();
    assert!((summary.minutes - 1.0).abs() < 1e-9, "{summary:?}");
    assert!((summary.reads - 3.0).abs() < 1e-9, "{summary:?}");
}

/// `summary` with each rate rounded to 1e-9, so sums of binary fractions compare.
fn rounded(summary: Summary) -> Summary {
    let r = |x: f64| (x * 1e9).round() / 1e9;
    Summary {
        minutes: r(summary.minutes),
        in_front: r(summary.in_front),
        busy: r(summary.busy),
        process_starts: r(summary.process_starts),
        git_starts: r(summary.git_starts),
        git_starts_per_agent: r(summary.git_starts_per_agent),
        turn_ends: (
            r(summary.turn_ends.0),
            r(summary.turn_ends.1),
            r(summary.turn_ends.2),
        ),
        reads: r(summary.reads),
        ceiling: r(summary.ceiling),
        ..summary
    }
}

#[test]
fn the_summary_reads_each_count_over_its_minutes() {
    let shell = (10, other(1));
    // One minute of listings 100 ms apart, one of them failed, and five survey reads, one of them
    // unanswered.
    let looks = (0..=600u64).map(|i| {
        let mut pids = vec![shell, (500, other(5))];
        if i >= 1 {
            // The first process born during the run, at 100 ms.
            pids.push((600, other(100)));
        }
        match i {
            10 => pids.push((700, git(200))),
            11..=15 => pids.push((710, git(210))),
            20 => pids.push((720, git(220))),
            _ => {}
        }
        if i >= 301 {
            // The last process born, at 30.1 s: 600 pids handed out since the first.
            pids.push((1_200, other(300)));
        }
        let mut look = if i == 450 {
            Look::default()
        } else {
            listing(&pids)
        };
        look.surveys = match i {
            0 => vec![read(
                0,
                &[
                    ("s-a", true, "busy"),
                    ("s-b", true, "idle"),
                    ("s-sh", false, "-"),
                ],
            )],
            50 => vec![read(5_000, &[("s-a", true, "busy"), ("s-b", true, "idle")])],
            100 => vec![(10_000, None)],
            150 => vec![read(
                15_000,
                &[
                    ("s-a", true, "idle"),
                    ("s-b", true, "idle"),
                    ("s-c", true, "busy"),
                ],
            )],
            290 => vec![read(
                29_000,
                &[
                    ("s-a", true, "busy"),
                    ("s-b", true, "idle"),
                    ("s-c", true, "idle"),
                ],
            )],
            _ => Vec::new(),
        };
        look
    });
    let in_front = 10.0 / 4.0;
    // A session shown from `first` to `last` was in front for that and one read period more.
    let period = SURVEY_EVERY.as_secs_f64() * 1_000.0;
    let per_minute_shown = |first: f64, last: f64| 60_000.0 / (last - first + period);
    let (s_a, s_c) = (
        per_minute_shown(0.0, 29_000.0),
        per_minute_shown(15_000.0, 29_000.0),
    );
    let expected = Summary {
        minutes: 1.0,
        listings: 600,
        failed: 1,
        mean_gap_ms: 100,
        max_gap_ms: 200,
        surveys: 4,
        unanswered: 1,
        sessions: 4,
        agents: 3,
        started: 1,
        in_front,
        busy: 4.0 / 10.0,
        // 600 pids over the 30 s from 100 ms to 30.1 s.
        process_starts: 1_200.0,
        git_starts: 3.0,
        git_starts_per_agent: 3.0 / in_front,
        // s-b none, s-a one and s-c one, each over the minutes it was shown: the median, the 95th
        // percentile and the most.
        turn_ends: (s_a, s_c, s_c),
        // s-a's turn end at 15 s, and s-c's first look; its turn end at 29 s falls within the
        // 20 s floor of that look.
        reads: 2.0,
        // Each agent session in front read once per floor, and s-c once at its start.
        ceiling: in_front / (FLOOR.as_secs_f64() / 60.0) + 1.0,
    };
    assert_eq!(
        rounded(run(looks).summary()),
        rounded(expected),
        "each count over the minutes it was sampled in"
    );
}

#[test]
fn a_survey_reply_reads_as_its_session_rows() {
    let body = "instance pid=7 host=h sessions=3 level=facts\n\
                session local=1 sid=s-a1 state=live program=claude program_kind=agent agent=busy agent_ms=4\n\
                session local=2 sid=s-b2 state=live program=zsh program_kind=shell agent=- agent_ms=9\n\
                session local=3 sid=s-c3 state=live program=claude program_kind=agent agent=wall:usage-session agent_ms=9\n\
                account id=x\n";
    assert_eq!(
        rows_of(body),
        [
            row("s-a1", true, "busy"),
            row("s-b2", false, "-"),
            row("s-c3", true, "wall:usage-session"),
        ]
    );
}

#[test]
fn a_survey_of_an_aterm_that_accepts_nothing_ends_unanswered() {
    // A stopped aterm: its socket listens, its token lies beside it, and nothing accepts.
    let dir = aterm_tempfile::Builder::new()
        .prefix("gs")
        .tempdir()
        .expect("a scratch dir");
    let sock = dir.path().join("x.sock");
    let listener = std::os::unix::net::UnixListener::bind(&sock).expect("a listening socket");
    std::fs::write(dir.path().join("x.sock.token"), "lab-token\n").expect("its token file");
    let sock = sock.to_str().expect("a UTF-8 path").to_owned();
    let (said, answer) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = said.send(survey_pass(Ok::<_, ()>(vec![(0, sock)]), instance_survey));
    });
    let read = answer
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the survey of a stopped aterm returned");
    assert_eq!(
        read, None,
        "a stopped aterm leaves the read unanswered, never taken as answered"
    );
    drop(listener);
}

/// `--read`'s folders: each agent row's `work_tree=`, else its `cwd=`, `~` taken as the home.
/// NEGATIVE CONTROL: a shell's row, an agent row naming neither or a relative path gives none.
#[test]
fn an_agent_rows_folder_is_its_work_tree_else_its_cwd() {
    let home = std::path::Path::new("/home/me");
    let body = "instance pid=7 host=h sessions=5 level=content\n\
                session sid=s-a1 program_kind=agent cwd=~/a work_tree=~/a/wt\n\
                session sid=s-b2 program_kind=agent cwd=%2Fsrv%2Fb work_tree=-\n\
                session sid=s-c3 program_kind=shell cwd=~/c\n\
                session sid=s-d4 program_kind=agent cwd=-\n\
                session sid=s-e5 program_kind=agent cwd=rel/e\n";
    assert_eq!(
        folders_of(body, home),
        [
            std::path::PathBuf::from("/home/me/a/wt"),
            std::path::PathBuf::from("/srv/b"),
        ]
    );
}

/// `--read`'s summary: the git starts per read and the wall time over the reads that gave a
/// record, and the counts of the others. NEGATIVE CONTROL: with no read that gave a record,
/// every figure is 0.
#[test]
fn the_read_summary_counts_starts_and_wall_time_per_read() {
    use aterm_agent::harness::work_record::{WorkRead, WorkRecord};
    let mut reads = Reads::default();
    for (starts, ms) in [(10, 120), (9, 80), (12, 400), (10, 100)] {
        let record = WorkRecord {
            starts,
            ..WorkRecord::default()
        };
        reads.fold(&WorkRead::Read(Box::new(record)), ms);
    }
    reads.fold(&WorkRead::None, 3);
    reads.fold(&WorkRead::Unread, 60_000);
    assert_eq!(
        reads.summary(),
        "reads: 4 gave a record, 1 had nothing to show, 1 did not finish\n\
         git starts per read: 9 fewest, 10 median, 12 most\n\
         wall time per read: p50 100 ms, p95 400 ms, 400 ms most\n"
    );
    assert!(
        Reads::default()
            .summary()
            .contains("git starts per read: 0 fewest, 0 median, 0 most"),
        "CONTROL: nothing read"
    );
}
