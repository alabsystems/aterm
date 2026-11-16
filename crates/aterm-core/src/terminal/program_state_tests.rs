// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

fn state(s: ProgramStateWord, k: Option<ProgramBlockedKind>) -> Option<Body> {
    Some(Body::Report(ProgramState {
        state: s,
        kind: k,
        app: ProgramApp::Codex,
    }))
}

#[test]
fn every_state_codex_sends_is_read() {
    use ProgramBlockedKind::*;
    use ProgramStateWord::*;
    assert_eq!(parse("state=idle:app=codex"), state(Idle, None));
    assert_eq!(parse("state=working:app=codex"), state(Working, None));
    assert_eq!(parse("state=blocked:app=codex"), state(Blocked, None));
    assert_eq!(parse("state=done:app=codex"), state(Done, None));
    assert_eq!(parse("state=error:app=codex"), state(Error, None));
    assert_eq!(
        parse("state=blocked:app=codex:kind=permission"),
        state(Blocked, Some(Permission))
    );
    assert_eq!(
        parse("state=blocked:app=codex:kind=question"),
        state(Blocked, Some(Question))
    );
    assert_eq!(
        parse("state=blocked:app=codex:kind=auth"),
        state(Blocked, Some(Auth))
    );
    assert_eq!(parse("state=clear"), Some(Body::Clear));
}

#[test]
fn a_body_this_reader_does_not_take_changes_nothing() {
    for body in [
        "",
        "state=",
        "state=sleeping:app=codex",
        "state=idle",
        "state=idle:app=",
        "state=idle:app=Claude-Code",
        "state=idle:app=codex:state=working",
        "state=blocked:app=codex:kind=coffee",
        "app=codex",
        "garbage",
    ] {
        assert_eq!(parse(body), None, "{body:?}");
        let mut report = ProgramStateRecord::default();
        assert!(!report.accept(body), "{body:?}");
        assert_eq!(report, ProgramStateRecord::default(), "{body:?}");
    }
    assert_eq!(parse(&"x".repeat(MAX_BODY_BYTES + 1)), None);
}

#[test]
fn a_kind_is_kept_only_on_blocked_and_an_unknown_key_is_left_alone() {
    assert_eq!(
        parse("state=idle:app=codex:kind=permission"),
        state(ProgramStateWord::Idle, None)
    );
    assert_eq!(
        parse("state=working:app=codex:v=2"),
        state(ProgramStateWord::Working, None)
    );
}

#[test]
fn the_sequence_moves_with_every_report_a_clear_and_a_reset() {
    let mut report = ProgramStateRecord::default();
    assert!(report.accept("state=working:app=codex"));
    assert_eq!(report.seq, 1);
    assert!(report.accept("state=working:app=codex"));
    assert_eq!(report.seq, 2, "a repeated report is still a report");
    assert!(report.accept("state=clear"));
    assert_eq!((report.seq, report.state), (3, None));
    report.reset();
    assert_eq!(report.seq, 3, "a reset with nothing standing moves nothing");
    assert!(report.accept("state=idle:app=codex"));
    report.reset();
    assert_eq!((report.seq, report.state), (5, None));
}

/// WHERE THE STANDING REPORT BEGAN: a report equal to the one standing (a writer's re-send)
/// keeps `began`; a different report, a kind that moved, a `clear` and a reset each begin
/// anew. NEGATIVE CONTROL: the re-sends, whose `seq` moves while `began` does not.
#[test]
fn a_report_sent_again_keeps_where_it_began() {
    let mut report = ProgramStateRecord::default();
    let mut take = |body: &str| {
        assert!(report.accept(body), "{body:?}");
        (report.seq, report.began)
    };
    assert_eq!(take("state=blocked:app=codex:kind=permission"), (1, 1));
    assert_eq!(
        take("state=blocked:app=codex:kind=permission"),
        (2, 1),
        "a re-send"
    );
    assert_eq!(
        take("state=blocked:app=codex:kind=question"),
        (3, 3),
        "the kind moved"
    );
    assert_eq!(take("state=working:app=codex"), (4, 4));
    assert_eq!(take("state=working:app=codex"), (5, 4), "a re-send");
    assert_eq!(take("state=clear"), (6, 6));
    assert_eq!(take("state=clear"), (7, 6), "a clear sent again");
    assert_eq!(take("state=blocked:app=codex"), (8, 8));
    report.reset();
    assert_eq!(
        (report.seq, report.began),
        (9, 9),
        "a reset begins none standing"
    );
}

/// A CARRIED REPORT: the standing report's body reads back as the same report, which
/// begins no turn, and a re-send of it keeps where it began; a body this reader does not
/// take, a `clear` and none leave none standing. NEGATIVE CONTROL: the same report taken
/// as a first report begins a turn.
#[test]
fn a_carried_report_stands_again_and_begins_no_turn() {
    let mut live = ProgramStateRecord::default();
    assert!(live.accept("state=blocked:app=codex:kind=permission"));
    assert_eq!(live.turns, 1, "CONTROL: a first report begins a turn");
    let body = live.carried();
    assert_eq!(
        body.as_deref(),
        Some("state=blocked:app=codex:kind=permission")
    );
    let mut restored = ProgramStateRecord::default();
    restored.restore(body.as_deref(), None);
    assert_eq!(restored.state, live.state);
    assert_eq!((restored.seq, restored.began, restored.turns), (1, 1, 0));
    assert!(restored.accept("state=blocked:app=codex:kind=permission"));
    assert_eq!(
        (restored.began, restored.turns),
        (1, 0),
        "a re-send of the carried box is the same box"
    );
    let numbering = (restored.seq, restored.began, restored.turns);
    restored.restore(body.as_deref(), None);
    assert_eq!((restored.seq, restored.began, restored.turns), numbering);
    restored.restore(body.as_deref(), None);
    assert_eq!((restored.seq, restored.began, restored.turns), numbering);
    assert!(restored.accept("state=working:app=codex"));
    assert_eq!(restored.turns, 0, "the carried turn goes on");
    for body in [None, Some("state=clear"), Some("state=sleeping:app=codex")] {
        let mut none = ProgramStateRecord::default();
        none.restore(body, None);
        assert_eq!(none, ProgramStateRecord::default(), "{body:?}");
    }
    let mut idle = ProgramStateRecord::default();
    assert!(idle.accept("state=idle:app=codex"));
    assert_eq!(idle.carried().as_deref(), Some("state=idle:app=codex"));
    idle.restore(None, None);
    assert_eq!((idle.seq, idle.began, idle.state), (2, 2, None));
}

/// What one report does to the turn edges ([`ProgramTurns`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Edge {
    /// Adds a turn it began.
    Begins,
    /// Adds a turn it resumed, which the count does not move for.
    Resumes,
    /// Ends the newest turn.
    Ends,
    /// Neither.
    Neither,
}

/// The reports the turn count and the turn edges are judged over: each body, the count after
/// it, what it does to the edges, and why.
const TURN_ROWS: [(&str, u64, Edge, &str); 17] = [
    (
        "state=working:app=codex",
        1,
        Edge::Begins,
        "the first report, in a turn",
    ),
    (
        "state=working:app=codex",
        1,
        Edge::Neither,
        "written again: no move",
    ),
    (
        "state=blocked:app=codex:kind=permission",
        1,
        Edge::Neither,
        "asked within it",
    ),
    (
        "state=working:app=codex",
        1,
        Edge::Neither,
        "answered: the same turn",
    ),
    ("state=done:app=codex", 1, Edge::Ends, "the turn ended"),
    (
        "state=working:app=codex",
        2,
        Edge::Begins,
        "a turn after done",
    ),
    ("state=idle:app=codex", 2, Edge::Ends, "ended"),
    (
        "state=blocked:app=codex:kind=question",
        3,
        Edge::Begins,
        "a turn that asked at once",
    ),
    ("state=error:app=codex", 3, Edge::Ends, "ended on an error"),
    (
        "state=working:app=codex",
        4,
        Edge::Begins,
        "a turn after an error",
    ),
    ("state=clear", 4, Edge::Neither, "a handoff mid-turn"),
    (
        "state=working:app=codex",
        4,
        Edge::Resumes,
        "the terminal taken back: resumed",
    ),
    ("state=done:app=codex", 4, Edge::Ends, "ended"),
    ("state=clear", 4, Edge::Neither, "a handoff at rest"),
    (
        "state=done:app=codex",
        4,
        Edge::Neither,
        "taken back: still at rest",
    ),
    ("state=clear", 4, Edge::Neither, "a handoff at rest"),
    (
        "state=working:app=codex",
        5,
        Edge::Begins,
        "a turn begun across it",
    ),
];

/// THE TURN COUNT: a turn begins at `working` or `blocked` after a report
/// at rest (`idle`, `done`, `error`) or as the first report; a report
/// written again, a move within a turn, and a report written again after
/// a `clear` or a full reset (the program took the terminal back) begin
/// none, while a turn that began across a `clear` does. NEGATIVE
/// CONTROLS: every line that begins none.
#[test]
fn the_turn_count_moves_only_where_a_turn_begins() {
    let mut report = ProgramStateRecord::default();
    let mut turns = |body: &str| {
        assert!(report.accept(body), "{body:?}");
        report.turns
    };
    for (body, want, _, why) in TURN_ROWS {
        assert_eq!(turns(body), want, "{why}: {body}");
    }
    report.reset();
    assert!(report.accept("state=working:app=codex"));
    assert_eq!(report.turns, 5, "resumed after a reset");
    assert!(report.accept("state=idle:app=codex"));
    report.reset();
    assert!(report.accept("state=working:app=codex"));
    assert_eq!(report.turns, 6, "begun across a reset");
    assert!(
        report.accept("state=idle:app=claude"),
        "another program's report stands"
    );
    assert!(report.accept("state=working:app=claude"));
    assert_eq!(report.turns, 6, "and begins no turn");
}

/// EACH TURN KEEPS THE BATCH TIMES OF ITS EDGES: a turn begins at its first `working`, dated
/// at the wall clock of the batch it arrived in, and ends at the next rest, dated at that
/// rest's batch and kept with its word; each is numbered in the order it was kept. NEGATIVE
/// CONTROLS: the re-send at 1005 and the box asked and answered at 1010 and 1012, none of
/// which begins or ends a turn.
#[test]
fn each_turn_keeps_the_batch_times_of_its_edges() {
    use ProgramStateWord::*;
    let edge = |seq, wall_ms| TurnEdge {
        seq,
        wall_ms: Some(wall_ms),
    };
    let turn = |n, began, ended| ProgramTurn {
        n,
        app: ProgramApp::Codex,
        began: Some(began),
        resumed: false,
        ended,
    };
    let mut report = ProgramStateRecord::default();
    for (body, wall_ms) in [
        ("state=working:app=codex", 1_000),
        ("state=working:app=codex", 1_005),
        ("state=blocked:app=codex:kind=permission", 1_010),
        ("state=working:app=codex", 1_012),
    ] {
        assert!(report.accept_at(body, Some(wall_ms)), "{body}");
    }
    assert_eq!(
        report.ring.iter().collect::<Vec<_>>(),
        [turn(1, edge(1, 1_000), None)],
        "one turn runs, begun at its first report"
    );
    for (body, wall_ms) in [
        ("state=done:app=codex", 1_017),
        ("state=working:app=codex", 2_000),
        ("state=error:app=codex", 2_030),
    ] {
        assert!(report.accept_at(body, Some(wall_ms)), "{body}");
    }
    assert_eq!(
        report.ring.iter().collect::<Vec<_>>(),
        [
            turn(1, edge(1, 1_000), Some((Done, edge(5, 1_017)))),
            turn(2, edge(6, 2_000), Some((Error, edge(7, 2_030)))),
        ]
    );
}

/// THE EDGES FOLLOW THE TURN COUNT: over the turn count's reports, each in a batch of its own
/// time, a turn is kept with its edge exactly where the count moves, a resumed one where an
/// in-turn report comes while no report stands (the count still), and the rows that end a
/// turn end the newest: the turn resumed across a `clear` ends on its resumed entry, the
/// `clear` having ended nothing. After a full reset a resumed turn ends on its own resumed
/// entry too, one begun across a reset is kept, and a report this reader does not take changes
/// nothing. NEGATIVE CONTROLS: every `Neither` row, `clear`s among them.
#[test]
fn the_edges_follow_the_turn_count() {
    fn step(
        report: &mut ProgramStateRecord,
        want: &mut Vec<ProgramTurn>,
        body: &str,
        wall_ms: u64,
        moves: Edge,
        why: &str,
    ) {
        let turns = report.turns;
        assert!(report.accept_at(body, Some(wall_ms)), "{why}: {body}");
        assert_eq!(
            moves == Edge::Begins,
            report.turns == turns + 1,
            "the count moves where a turn begins: {why}: {body}"
        );
        let edge = TurnEdge {
            seq: report.seq,
            wall_ms: Some(wall_ms),
        };
        let n = want.len() as u64 + 1;
        match moves {
            Edge::Begins | Edge::Resumes => want.push(ProgramTurn {
                n,
                app: ProgramApp::Codex,
                began: Some(edge),
                resumed: moves == Edge::Resumes,
                ended: None,
            }),
            Edge::Ends => {
                let word = report.state.map(|state| state.state);
                let newest = want.last_mut().expect("a turn to end");
                assert_eq!(newest.ended, None, "{why}: {body}");
                newest.ended = word.map(|word| (word, edge));
            }
            Edge::Neither => {}
        }
        assert_eq!(
            report.ring.iter().collect::<Vec<_>>(),
            *want,
            "{why}: {body}"
        );
    }
    let mut report = ProgramStateRecord::default();
    let mut want = Vec::new();
    let mut wall_ms = 1_000;
    for (body, _, moves, why) in TURN_ROWS {
        wall_ms += 10;
        step(&mut report, &mut want, body, wall_ms, moves, why);
    }
    report.reset();
    let after_reset = [
        (
            "state=working:app=codex",
            Edge::Resumes,
            "resumed after a reset",
        ),
        ("state=idle:app=codex", Edge::Ends, "the resumed turn ended"),
    ];
    for (body, moves, why) in after_reset {
        wall_ms += 10;
        step(&mut report, &mut want, body, wall_ms, moves, why);
    }
    report.reset();
    wall_ms += 10;
    let why = "begun across a reset";
    step(
        &mut report,
        &mut want,
        "state=working:app=codex",
        wall_ms,
        Edge::Begins,
        why,
    );
    assert_eq!(want.len(), 8, "six turns ran, two of them resumed once");
    let kept: Vec<_> = report.ring.iter().collect();
    assert!(report.accept_at("state=idle:app=claude", Some(wall_ms + 10)));
    assert_eq!(report.ring.iter().collect::<Vec<_>>(), kept);
}

/// A TURN RE-ENTERED WHILE NO REPORT STANDS IS A RESUMED ENTRY: codex-alab clears its report
/// before a job-control stop or an external program and sends it again after, so a `clear`
/// mid-turn then `working` keeps a resumed entry at the `working`'s batch time, and the next
/// rest ends that entry, the one that began the turn staying unended; a full reset mid-turn
/// then `working` does the same. Neither moves the turn count. CONTROL: a `clear` at rest then
/// `working` is a real beginning, which the count moves for.
#[test]
fn a_turn_reentered_while_no_report_stands_is_a_resumed_entry() {
    use ProgramStateWord::*;
    for reset in [false, true] {
        let mut report = ProgramStateRecord::default();
        assert!(report.accept_at("state=working:app=codex", Some(1_000)));
        if reset {
            report.reset();
        } else {
            assert!(report.accept_at("state=clear", Some(1_100)));
        }
        assert!(report.accept_at("state=working:app=codex", Some(1_200)));
        assert!(report.accept_at("state=done:app=codex", Some(1_300)));
        let kept: Vec<_> = report.ring.iter().collect();
        assert_eq!(report.turns, 1, "{reset}: one turn counted");
        assert!(
            matches!(kept.as_slice(), [first, resumed]
                if !first.resumed && first.ended.is_none()
                    && first.began.and_then(|b| b.wall_ms) == Some(1_000)
                    && resumed.resumed
                    && resumed.began.and_then(|b| b.wall_ms) == Some(1_200)
                    && resumed.ended.map(|(w, e)| (w, e.wall_ms)) == Some((Done, Some(1_300)))),
            "{reset}: {kept:#?}"
        );
    }
    let mut rest = ProgramStateRecord::default();
    assert!(rest.accept_at("state=done:app=codex", Some(1_000)));
    assert!(rest.accept_at("state=clear", Some(1_100)));
    assert!(rest.accept_at("state=working:app=codex", Some(1_200)));
    assert_eq!(
        rest.turns, 1,
        "CONTROL: a turn begun across a clear at rest"
    );
    assert!(
        rest.ring.newest().is_some_and(|t| !t.resumed),
        "CONTROL: {:?}",
        rest.ring.newest()
    );
}

/// A CLEAR IS NUMBERED SO A MOVE CAN SPLIT ITS WRITERS: `cleared` is the sequence of the
/// latest `clear`, and of a full reset that took a report down; a reset with nothing standing
/// leaves it. A reader's cursor reads only the edges past it, and the turns kept past it that
/// rotated out are counted lost. NEGATIVE CONTROL: a reader at the head reads nothing and
/// loses nothing.
#[test]
fn a_clear_is_numbered_so_a_move_can_split_its_writers() {
    let mut report = ProgramStateRecord::default();
    assert!(report.accept("state=working:app=codex"));
    assert_eq!(report.cleared, 0);
    assert!(report.accept("state=clear"));
    assert_eq!(report.cleared, 2, "the clear's own sequence");
    report.reset();
    assert_eq!(report.cleared, 2, "a reset with nothing standing");
    assert!(report.accept("state=working:app=codex"));
    report.reset();
    assert_eq!(report.cleared, 4, "a reset that took a report down");
    let mut out = Vec::new();
    let head = report.ring.head(report.seq);
    assert_eq!(report.ring.after(Some(head), &mut out), 0, "CONTROL");
    assert!(out.is_empty(), "CONTROL: {out:?}");
    assert!(report.accept("state=done:app=codex"));
    assert_eq!(report.ring.after(Some(head), &mut out), 0);
    assert!(
        matches!(out.as_slice(), [t] if t.resumed && t.ended.is_some()),
        "the resumed entry, read for its new end: {out:?}"
    );
    let head = report.ring.head(report.seq);
    for _ in 0..PROGRAM_TURNS_KEPT + 3 {
        assert!(report.accept("state=working:app=codex"));
        assert!(report.accept("state=done:app=codex"));
    }
    assert_eq!(
        report.ring.after(Some(head), &mut out),
        3,
        "three rotated out"
    );
    assert_eq!(
        out.len(),
        PROGRAM_TURNS_KEPT,
        "the newest kept, each past the cursor"
    );
    assert_eq!(
        out.first().map(|t| t.n),
        Some(head.n + 4),
        "the oldest kept is the fourth past the cursor"
    );
}

/// THE RING KEEPS THE NEWEST 32 AND NUMBERS EVERY TURN: of 40 turns the newest 32 are kept,
/// oldest first, numbered 9 to 40. NEGATIVE CONTROL: restoring the report standing keeps them.
#[test]
fn the_ring_keeps_the_newest_32_and_numbers_every_turn() {
    let mut report = ProgramStateRecord::default();
    for turn in 0..40_u64 {
        assert!(report.accept_at("state=working:app=codex", Some(1_000 * turn)));
        assert!(report.accept_at("state=done:app=codex", Some(1_000 * turn + 17)));
    }
    let kept: Vec<_> = report.ring.iter().collect();
    assert_eq!(
        kept.iter().map(|t| t.n).collect::<Vec<_>>(),
        (9..=40).collect::<Vec<_>>(),
        "the newest 32, oldest first"
    );
    assert!(
        kept.iter()
            .all(|t| t.began.and_then(|b| b.wall_ms) == Some(1_000 * (t.n - 1))),
        "{kept:?}"
    );
    let body = report.carried();
    report.restore(body.as_deref(), None);
    assert_eq!(
        report.ring.iter().collect::<Vec<_>>(),
        kept,
        "CONTROL: the report standing restored keeps them"
    );
}

/// A RESTORED IN-TURN REPORT IS A RESUMED ENTRY: restoring a different report drops the kept
/// turns and, its report in a turn, keeps one resumed at the restore's sequence with no wall
/// clock, which the next rest ends and which begins no turn. NEGATIVE CONTROL: a restored
/// report at rest keeps none.
#[test]
fn a_restored_in_turn_report_is_a_resumed_entry() {
    let mut report = ProgramStateRecord::default();
    for turn in 0..3_u64 {
        assert!(report.accept_at("state=working:app=codex", Some(1_000 * turn)));
        assert!(report.accept_at("state=done:app=codex", Some(1_000 * turn + 17)));
    }
    report.restore(Some("state=working:app=codex"), None);
    let restored = report.seq;
    assert_eq!(
        report.ring.iter().collect::<Vec<_>>(),
        [ProgramTurn {
            n: 4,
            app: ProgramApp::Codex,
            began: Some(TurnEdge {
                seq: restored,
                wall_ms: None,
            }),
            resumed: true,
            ended: None,
        }],
        "the kept turns dropped, the installed one resumed"
    );
    assert!(report.accept_at("state=done:app=codex", Some(7_017)));
    assert!(
        report
            .ring
            .newest()
            .is_some_and(|t| t.n == 4 && t.ended.map(|(_, e)| e.wall_ms) == Some(Some(7_017))),
        "the rest ends it: {:?}",
        report.ring.newest()
    );
    assert_eq!(report.turns, 3, "and begins none");
    let mut rest = ProgramStateRecord::default();
    rest.restore(Some("state=done:app=codex"), None);
    assert_eq!(rest.ring.iter().count(), 0, "CONTROL");
}

/// THE COUNT THROUGH THE PARSER, the bytes codex-alab writes: a turn
/// that begins and ends between two reads of the engine leaves the same
/// report standing (`done`) and the count one higher; a handoff mid-turn
/// (`clear`, then `working` again) leaves it as it was.
#[test]
fn a_turn_between_two_reads_moves_the_count_and_a_handoff_does_not() {
    use crate::terminal::Terminal;
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b]7501;state=done:app=codex\x1b\\");
    let before = term.program_state();
    term.process(b"\x1b]7501;state=working:app=codex\x1b\\");
    term.process(b"\x1b]7501;state=done:app=codex\x1b\\");
    let after = term.program_state();
    assert_eq!(after.state, before.state, "the same report stands");
    assert_eq!(after.turns, before.turns + 1, "and a turn ran");
    term.process(b"\x1b]7501;state=working:app=codex\x1b\\");
    term.process(b"\x1b]7501;state=clear\x1b\\");
    term.process(b"\x1b]7501;state=working:app=codex\x1b\\");
    assert_eq!(term.program_state().turns, after.turns + 1, "one turn");
}

#[test]
fn the_wire_word_carries_the_kind() {
    let blocked = ProgramState {
        state: ProgramStateWord::Blocked,
        kind: Some(ProgramBlockedKind::Permission),
        app: ProgramApp::Codex,
    };
    assert_eq!(blocked.wire(), "blocked:permission");
    let idle = ProgramState {
        state: ProgramStateWord::Idle,
        kind: None,
        app: ProgramApp::GrokBuild,
    };
    assert_eq!(idle.wire(), "idle");
}

/// The bytes as the two codex builds write them, through the real parser:
/// vendor 0.163's format string (`\x1b]7501;state={}:app=codex\x1b\\`,
/// read out of its shipped binary: idle, working and blocked, no kind)
/// and codex-alab main's `SetTerminalProgramStatus` (adds done, error and
/// a blocked report's kind), then `ClearTerminalProgramStatus`. A full
/// reset clears what stands; a body with a `;` in it is no report.
#[test]
fn the_bytes_codex_writes_are_read_through_the_parser() {
    use crate::terminal::Terminal;
    use ProgramBlockedKind::*;
    use ProgramStateWord::*;
    let mut term = Terminal::new(24, 80);
    let seen = |term: &mut Terminal, bytes: &[u8]| {
        term.process(bytes);
        term.program_state().state
    };
    let vendor: [(&[u8], ProgramStateWord); 3] = [
        (b"\x1b]7501;state=idle:app=codex\x1b\\", Idle),
        (b"\x1b]7501;state=working:app=codex\x1b\\", Working),
        (b"\x1b]7501;state=blocked:app=codex\x1b\\", Blocked),
    ];
    for (bytes, want) in vendor {
        assert_eq!(
            seen(&mut term, bytes),
            Some(ProgramState {
                state: want,
                kind: None,
                app: ProgramApp::Codex,
            })
        );
    }
    let alab: [(&[u8], ProgramStateWord, Option<ProgramBlockedKind>); 5] = [
        (b"\x1b]7501;state=done:app=codex\x1b\\", Done, None),
        (b"\x1b]7501;state=error:app=codex\x1b\\", Error, None),
        (
            b"\x1b]7501;state=blocked:app=codex:kind=permission\x1b\\",
            Blocked,
            Some(Permission),
        ),
        (
            b"\x1b]7501;state=blocked:app=codex:kind=question\x1b\\",
            Blocked,
            Some(Question),
        ),
        (
            b"\x1b]7501;state=blocked:app=codex:kind=auth\x07",
            Blocked,
            Some(Auth),
        ),
    ];
    for (bytes, want, kind) in alab {
        assert_eq!(
            seen(&mut term, bytes),
            Some(ProgramState {
                state: want,
                kind,
                app: ProgramApp::Codex,
            })
        );
    }
    let before = term.program_state().seq;
    assert_eq!(seen(&mut term, b"\x1b]7501;state=clear\x1b\\"), None);
    assert_eq!(term.program_state().seq, before + 1);
    seen(
        &mut term,
        b"\x1b]21337;status=Working;indicator=#ff9500;status-color=;detail=\x07",
    );
    assert_eq!(term.program_state().seq, before + 1);
    assert_eq!(
        seen(&mut term, b"\x1b]7501;state=idle;app=codex\x1b\\"),
        None
    );
    assert_eq!(term.program_state().seq, before + 1);
    seen(&mut term, b"\x1b]7501;state=working:app=codex\x1b\\");
    assert_eq!(seen(&mut term, b"\x1bc"), None);
    assert_eq!(term.program_state().seq, before + 3);
}

/// The bytes Grok Build 1.0.70 writes, through the real parser. Its binary holds the pieces
/// of one report, `ESC ] 7501;state=` `:kind=permission` `:app=grok-build` `:title=`, its
/// state words `working`, `blocked` and `error`, a child record's clear
/// (`state=clear:id=`) and, at exit, `state=clear`; its question and sign-in waits name no
/// kind (it holds no other). Its title is base64, with or without padding.
#[test]
fn the_root_record_grok_build_writes_is_read_through_the_parser() {
    use crate::terminal::Terminal;
    use ProgramStateWord::*;
    let grok = |state, kind| {
        Some(ProgramState {
            state,
            kind,
            app: ProgramApp::GrokBuild,
        })
    };
    let mut term = Terminal::new(24, 80);
    let rows: [(&[u8], Option<ProgramState>); 6] = [
        (
            b"\x1b]7501;state=working:app=grok-build:title=Rml4IHRoZSBwYXJzZXI\x1b\\",
            grok(Working, None),
        ),
        (
            b"\x1b]7501;state=blocked:kind=permission:app=grok-build:title=Rml4IHRoZSBwYXJzZXI=\x1b\\",
            grok(Blocked, Some(ProgramBlockedKind::Permission)),
        ),
        (
            b"\x1b]7501;state=blocked:app=grok-build:title=Rml4IHRoZSBwYXJzZXI\x1b\\",
            grok(Blocked, None),
        ),
        (
            b"\x1b]7501;state=done:app=grok-build:title=Rml4IHRoZSBwYXJzZXI\x07",
            grok(Done, None),
        ),
        (
            b"\x1b]7501;state=error:app=grok-build\x1b\\",
            grok(Error, None),
        ),
        (b"\x1b]7501;state=clear\x1b\\", None),
    ];
    for (bytes, want) in rows {
        term.process(bytes);
        assert_eq!(
            term.program_state().state,
            want,
            "{}",
            String::from_utf8_lossy(bytes)
        );
    }
    assert_eq!(term.program_state().turns, 1, "one turn, working to done");
}

/// A CHILD RECORD SAYS NOTHING OF THE SESSION: Grok Build gives each running subagent its own
/// record (`id=<subagent>`), says it done when it finishes, and clears it with
/// `state=clear:id=<subagent>`, all while the session's own turn runs. None of those is
/// taken: the root `working` stands, its sequence does not move, and no turn ends or begins.
/// NEGATIVE CONTROL: the same reports with no `id` are taken.
#[test]
fn a_child_records_report_and_clear_leave_the_root_record_standing() {
    use crate::terminal::Terminal;
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b]7501;state=working:app=grok-build\x1b\\");
    let root = term.program_state();
    for child in [
        &b"\x1b]7501;state=working:app=grok-build:id=sub_1:title=cmVhZA\x1b\\"[..],
        b"\x1b]7501;state=blocked:kind=permission:app=grok-build:id=sub_1\x1b\\",
        b"\x1b]7501;state=done:app=grok-build:id=sub_1\x1b\\",
        b"\x1b]7501;state=clear:id=sub_1\x1b\\",
        b"\x1b]7501;state=clear:id=s/sub_2\x1b\\",
    ] {
        term.process(child);
        assert_eq!(
            term.program_state(),
            root,
            "{}",
            String::from_utf8_lossy(child)
        );
    }
    term.process(b"\x1b]7501;state=done:app=grok-build\x1b\\");
    assert_eq!(
        term.program_state().state.map(|s| s.state),
        Some(ProgramStateWord::Done),
        "the root's own report is taken"
    );
    term.process(b"\x1b]7501;state=clear\x1b\\");
    assert_eq!(term.program_state().state, None, "the root's own clear is");
}

/// A CHECKPOINT CARRIES THE REPORT'S OWN PROGRAM: Grok Build's standing report is carried as
/// `app=grok-build` and restored as the same report, a re-send of it keeping where it began.
/// NEGATIVE CONTROL: codex's carries `app=codex`.
#[test]
fn a_carried_grok_build_report_restores_as_grok_builds() {
    let mut live = ProgramStateRecord::default();
    assert!(live.accept("state=blocked:kind=permission:app=grok-build:title=cGFyc2Vy"));
    let body = live.carried();
    assert_eq!(
        body.as_deref(),
        Some("state=blocked:app=grok-build:kind=permission")
    );
    let mut restored = ProgramStateRecord::default();
    restored.restore(body.as_deref(), None);
    assert_eq!(restored.state, live.state);
    assert!(restored.accept("state=blocked:kind=permission:app=grok-build"));
    assert_eq!((restored.began, restored.turns), (1, 0), "the same box");
    let mut codex = ProgramStateRecord::default();
    assert!(codex.accept("state=working:app=codex"));
    assert_eq!(codex.carried().as_deref(), Some("state=working:app=codex"));
}

/// A report as long as the protocol allows is a report: Grok Build's carries its title, up
/// to 256 base64 bytes, beside a `msg` of up to 2,732. One byte past the cap is none.
#[test]
fn a_report_with_a_title_and_message_at_the_protocols_caps_is_taken() {
    let title = "A".repeat(256);
    let msg = "B".repeat(2732);
    let body =
        format!("state=blocked:kind=permission:app=grok-build:progress=40:title={title}:msg={msg}");
    assert_eq!(
        parse(&body),
        Some(Body::Report(ProgramState {
            state: ProgramStateWord::Blocked,
            kind: Some(ProgramBlockedKind::Permission),
            app: ProgramApp::GrokBuild,
        }))
    );
    let pad = MAX_BODY_BYTES - "state=done:app=grok-build:msg=".len();
    let edge = format!("state=done:app=grok-build:msg={}", "C".repeat(pad));
    assert_eq!(edge.len(), MAX_BODY_BYTES);
    assert!(parse(&edge).is_some(), "at the cap");
    assert_eq!(parse(&format!("{edge}C")), None, "past it");
}

/// CLAUDE CODE'S OWN REPORTS ARE READ: the root records Claude Code 2.1.323
/// writes once its query is answered (MEASURED 2026-11-12 in the real-Claude lab), through the
/// real parser: `idle` at once, a turn `working` that failed (`error` with its base64 `msg`), a
/// turn `working` to `done`, a turn `working` that stopped on a box (`blocked:kind=permission`
/// with the box's `msg`) and went back to `idle` when it was denied, and `clear` at exit. Three
/// turns. NEGATIVE CONTROL: `app=claude`, a word no program writes, is still not taken.
#[test]
fn the_reports_claude_code_writes_are_read_through_the_parser() {
    use crate::terminal::Terminal;
    use ProgramStateWord::*;
    let claude = |state, kind| {
        Some(ProgramState {
            state,
            kind,
            app: ProgramApp::ClaudeCode,
        })
    };
    let mut term = Terminal::new(24, 80);
    let rows: [(&[u8], Option<ProgramState>); 9] = [
        (
            b"\x1b]7501;state=idle:app=claude-code\x07",
            claude(Idle, None),
        ),
        (
            b"\x1b]7501;state=working:app=claude-code\x07",
            claude(Working, None),
        ),
        (
            b"\x1b]7501;state=error:app=claude-code:msg=QVBJIEVycm9yOiBBUEkgcmV0dXJuZWQgYW4gZW1wdHkgb3IgbWFsZm9ybWVkIHJlc3BvbnNl\x07",
            claude(Error, None),
        ),
        (
            b"\x1b]7501;state=working:app=claude-code\x07",
            claude(Working, None),
        ),
        (
            b"\x1b]7501;state=done:app=claude-code\x07",
            claude(Done, None),
        ),
        (
            b"\x1b]7501;state=working:app=claude-code\x07",
            claude(Working, None),
        ),
        (
            b"\x1b]7501;state=blocked:app=claude-code:kind=permission:msg=YXBwcm92ZSBCYXNoOiBybSAtcmYgL3ByaXZhdGUvdG1wL2F0LkRkV1ovcHJvai9ub3RoaW5nLWhlcmU=\x07",
            claude(Blocked, Some(ProgramBlockedKind::Permission)),
        ),
        (
            b"\x1b]7501;state=idle:app=claude-code\x07",
            claude(Idle, None),
        ),
        (b"\x1b]7501;state=clear\x07", None),
    ];
    for (bytes, want) in rows {
        term.process(bytes);
        assert_eq!(
            term.program_state().state,
            want,
            "{}",
            String::from_utf8_lossy(bytes)
        );
    }
    assert_eq!(
        term.program_state().turns,
        3,
        "three turns, each ended at a rest"
    );
    let ends: Vec<_> = term
        .program_turns()
        .iter()
        .map(|t| (t.app, t.ended.map(|(w, _)| w)))
        .collect();
    assert_eq!(
        ends,
        [
            (ProgramApp::ClaudeCode, Some(Error)),
            (ProgramApp::ClaudeCode, Some(Done)),
            (ProgramApp::ClaudeCode, Some(Idle)),
        ]
    );
    // NEGATIVE CONTROL: `app=claude` is another program's word, not Claude Code's: it stands
    // apart and draws no turn.
    let mut control = ProgramStateRecord::default();
    assert!(control.accept("state=working:app=claude"));
    assert_eq!((control.seq, control.turns, control.state), (0, 0, None));
    assert!(control.other.state.is_some_and(|s| !s.app.is_known()));
}

/// A CHECKPOINT CARRIES CLAUDE CODE'S REPORT AS CLAUDE CODE'S: its standing box is carried as
/// `app=claude-code` and restored as the same report, a re-send of it keeping where it began and
/// beginning no turn, so a restore counts none of its turns twice.
#[test]
fn a_carried_claude_code_report_restores_as_claude_codes() {
    let mut live = ProgramStateRecord::default();
    assert!(live.accept("state=blocked:app=claude-code:kind=permission:msg=YXBwcm92ZQ=="));
    let body = live.carried();
    assert_eq!(
        body.as_deref(),
        Some("state=blocked:app=claude-code:kind=permission")
    );
    let mut restored = ProgramStateRecord::default();
    restored.restore(body.as_deref(), None);
    assert_eq!(restored.state, live.state);
    assert!(restored.accept("state=blocked:app=claude-code:kind=permission:msg=YXBwcm92ZQ=="));
    assert_eq!((restored.began, restored.turns), (1, 0), "the same box");
}

/// GROK BUILD 1.0.77 NAMES ITSELF `grok`: once its query is answered it writes
/// `state=idle:app=grok` and, at exit, `state=clear` (MEASURED 2026-11-12), where 1.0.70's binary
/// holds `app=grok-build`. Both are Grok Build's, carried under its one wire word.
#[test]
fn grok_build_1_0_77s_app_word_is_grok_builds() {
    use crate::terminal::Terminal;
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b]7501;state=idle:app=grok\x1b\\");
    assert_eq!(
        term.program_state().state,
        Some(ProgramState {
            state: ProgramStateWord::Idle,
            kind: None,
            app: ProgramApp::GrokBuild,
        })
    );
    let mut live = ProgramStateRecord::default();
    assert!(live.accept("state=working:app=grok"));
    assert_eq!(
        live.carried().as_deref(),
        Some("state=working:app=grok-build")
    );
    term.process(b"\x1b]7501;state=clear\x1b\\");
    assert_eq!(term.program_state().state, None);
}

/// A REPORT IS A RUN SIGN ONLY WHILE ITS WRITER'S GROUP HOLDS THE FRONT: a Claude Code killed
/// mid-turn leaves `working` standing, and nothing it starts after it overwrites it (a shell
/// writes none, nor does a Claude Code whose title is off). Once the host says the bytes it
/// parses are another group's, the report is no run sign, while a checkpoint still carries it; a
/// program stopped and resumed holds the front again and its report is its sign again, and the
/// next program's own report is one. A report taken before any group was said is the first
/// group's said, and a restore installs its report as the group's in front.
#[test]
fn a_report_is_a_run_sign_only_while_its_writers_group_holds_the_front() {
    use crate::terminal::Terminal;
    let working = Some("state=working:app=claude-code");
    let idle = Some("state=idle:app=claude-code");
    let sign = |t: &Terminal| t.program_state().run_sign();
    let mut term = Terminal::new(24, 80);
    term.program_front_is(100);
    term.process(b"\x1b]7501;state=working:app=claude-code\x07");
    assert_eq!(sign(&term).as_deref(), working, "its writer's bytes");
    term.program_front_is(100);
    assert_eq!(sign(&term).as_deref(), working, "the same group");
    term.program_front_is(0);
    term.program_front_is(-1);
    assert_eq!(sign(&term).as_deref(), working, "no group says nothing");
    term.program_front_is(200);
    assert_eq!(sign(&term), None, "another group in front");
    assert_eq!(
        term.program_state().carried().as_deref(),
        working,
        "a checkpoint still carries it"
    );
    term.program_front_is(100);
    assert_eq!(
        sign(&term).as_deref(),
        working,
        "its writer resumed in front"
    );
    term.program_front_is(300);
    assert_eq!(sign(&term), None, "killed: a later group in front");
    term.process(b"\x1b]7501;state=idle:app=claude-code\x07");
    assert_eq!(
        sign(&term).as_deref(),
        idle,
        "the next program's own report"
    );
    term.program_front_is(400);
    assert_eq!(sign(&term), None, "its writer left in turn");
    let mut control = Terminal::new(24, 80);
    control.process(b"\x1b]7501;state=working:app=claude-code\x07");
    assert_eq!(
        sign(&control).as_deref(),
        working,
        "with no group said, the report standing is the sign"
    );
    control.program_front_is(100);
    assert_eq!(
        sign(&control).as_deref(),
        working,
        "taken before any group was said: the first group's said"
    );
    control.program_front_is(200);
    assert_eq!(sign(&control), None, "another group in front");
    let mut restored = ProgramStateRecord::default();
    restored.front_is(100);
    restored.accept("state=working:app=claude-code");
    restored.front_is(200);
    assert_eq!(restored.run_sign(), None, "its writer left");
    restored.restore(Some("state=idle:app=claude-code"), None);
    assert_eq!(
        restored.run_sign().as_deref(),
        idle,
        "a restore installs its report as the group's in front"
    );
}

/// A report's app owns its turn even when no clear separates two supported programs.
/// The same state words from another app cannot end or resume the earlier app's turn.
#[test]
fn a_report_from_another_app_never_ends_or_resumes_the_previous_apps_turn() {
    for (first, next) in [
        ("codex", "grok-build"),
        ("grok-build", "codex"),
        ("codex", "claude-code"),
        ("claude-code", "codex"),
        ("grok-build", "claude-code"),
        ("claude-code", "grok-build"),
    ] {
        for clear in [false, true] {
            let began = format!("state=working:app={first}");
            let mut record = ProgramStateRecord::default();
            assert!(record.accept_at(&began, Some(10)));
            if clear {
                assert!(record.accept("state=clear"));
            }
            assert!(record.accept_at(&format!("state=error:app={next}"), Some(20)));
            let turns: Vec<_> = record.turns().iter().collect();
            assert_eq!(turns.len(), 1, "{first} to {next}, clear={clear}");
            assert!(
                turns[0].ended.is_none(),
                "another app ended {first}: {turns:?}"
            );

            let mut record = ProgramStateRecord::default();
            assert!(record.accept_at(&began, Some(10)));
            if clear {
                assert!(record.accept("state=clear"));
            }
            assert!(record.accept_at(&format!("state=working:app={next}"), Some(20)));
            assert!(record.accept_at(&format!("state=done:app={next}"), Some(30)));
            let turns: Vec<_> = record.turns().iter().collect();
            assert_eq!(
                turns.len(),
                2,
                "{first} to {next}, clear={clear}: {turns:?}"
            );
            assert_eq!(
                record.report().turns,
                2,
                "a different app begins its own turn"
            );
            assert!(
                turns[0].ended.is_none(),
                "the earlier app has no reported end"
            );
            assert!(
                !turns[1].resumed,
                "the new app resumes no earlier app's turn"
            );
            assert_ne!(turns[0].app, turns[1].app);
            assert_eq!(turns[1].began.and_then(|e| e.wall_ms), Some(20));
            assert_eq!(turns[1].ended.map(|(_, e)| e.wall_ms), Some(Some(30)));
        }
    }
}

/// Claude Code's root lifecycle survives the real parser and checkpoint. Child reports,
/// unknown app spellings and a query are negative controls: none changes the root.
#[test]
fn claude_code_root_reports_survive_parsing_and_checkpoint() {
    use crate::terminal::Terminal;
    let mut term = Terminal::new(24, 80);
    for word in ["idle", "working", "blocked", "done", "error"] {
        term.process(format!("\x1b]7501;state={word}:app=claude-code\x1b\\").as_bytes());
        let report = term.program_state();
        let state = report.state.expect("Claude Code root report");
        assert_eq!(state.app, ProgramApp::ClaudeCode);
        assert_eq!(state.state.as_str(), word);
        for body in [
            "state=working:app=claude-code:id=child",
            "state=clear:app=claude-code:id=child",
            "state=working:app=Claude",
            "?",
        ] {
            term.process(format!("\x1b]7501;{body}\x1b\\").as_bytes());
            assert_eq!(term.program_state(), report, "{body}");
        }
        let restored = Terminal::from_checkpoint(&term.checkpoint());
        assert_eq!(restored.program_state().state, Some(state));
    }
    term.process(b"\x1b]7501;state=clear:app=claude-code\x1b\\");
    assert_eq!(term.program_state().state, None);
}

/// ANOTHER PROGRAM'S WORD IS KEPT TO BE SHOWN: a report naming a program this reader does not
/// know (`app=kimi-code`, `app=claude`, a word at the 32-byte bound) stands apart with that word
/// ([`ProgramStateReport::other`], its own sequence moving), and its checkpoint body restores it
/// there; it moves none of the known programs' record, so a codex `working` it follows still
/// stands and its turn runs on, and a known program's next report takes it away. A known word is
/// never another program's. NEGATIVE CONTROL: the same reports from `app=codex` stand as the
/// known report and draw their turn.
#[test]
fn an_unknown_app_word_is_kept_for_display() {
    let at_bound = "a".repeat(AppWord::MAX);
    for word in ["kimi-code", "claude", "my.agent_2", at_bound.as_str()] {
        let other = ProgramApp::Other(AppWord::new(word).expect(word));
        assert_eq!(other.as_str(), word);
        assert!(!other.is_known(), "{word}");
        let mut report = ProgramStateRecord::default();
        assert!(
            report.accept(&format!("state=working:app={word}")),
            "{word}"
        );
        assert_eq!(
            report.other.state,
            Some(ProgramState {
                state: ProgramStateWord::Working,
                kind: None,
                app: other,
            }),
            "{word}"
        );
        assert!(report.accept(&format!("state=blocked:app={word}:kind=question")));
        assert_eq!(report.other.seq, 2, "{word}: its own sequence moves");
        assert_eq!(report.carried(), None, "{word}: no known report stands");
        let carried = report.carried_other();
        assert_eq!(
            carried.as_deref(),
            Some(format!("state=blocked:app={word}:kind=question").as_str())
        );
        let mut restored = ProgramStateRecord::default();
        restored.restore(None, carried.as_deref());
        assert_eq!(restored.other.state, report.other.state, "{word}: carried");
        assert!(report.accept(&format!("state=done:app={word}")));
        let mut known = ProgramStateRecord::default();
        known.report.other = report.other;
        assert_eq!(
            report, known,
            "{word}: the known programs' record never moved"
        );
        assert_eq!(restored.state, None, "{word}: nor did the restored one's");
    }
    for known in ["claude-code", "codex", "grok-build"] {
        assert_eq!(AppWord::new(known), None, "{known} is a known program");
    }
    let mut codex = ProgramStateRecord::default();
    assert!(codex.accept("state=working:app=codex"));
    let working = codex.state;
    assert!(codex.accept("state=idle:app=kimi-code"));
    assert_eq!(
        codex.state, working,
        "another program's rest leaves codex working"
    );
    assert_eq!(codex.seq, 1);
    assert!(codex.accept("state=working:app=codex"));
    assert_eq!(
        codex.other.state, None,
        "a known report takes another's away"
    );
    assert_eq!(codex.turns, 1, "the codex turn ran on");
    assert!(codex.accept("state=done:app=codex"));
    assert_eq!(
        (codex.turns, codex.turns().iter().count()),
        (1, 1),
        "CONTROL: a known program's reports stand and draw their turn"
    );
    assert!(codex.accept("state=idle:app=kimi-code"));
    let (known, other) = (codex.carried(), codex.carried_other());
    assert_eq!(
        (known.as_deref(), other.as_deref()),
        (
            Some("state=done:app=codex"),
            Some("state=idle:app=kimi-code")
        ),
        "a checkpoint carries both"
    );
    let mut adopted = ProgramStateRecord::default();
    adopted.restore(known.as_deref(), other.as_deref());
    assert_eq!(
        (adopted.state, adopted.other.state),
        (codex.state, codex.other.state),
        "each restored to its own slot"
    );
    let mut crossed = ProgramStateRecord::default();
    crossed.restore(other.as_deref(), known.as_deref());
    assert_eq!(
        (crossed.state, crossed.other.state),
        (None, None),
        "a body in the other kind's slot is installed nowhere"
    );
    codex.reset();
    assert_eq!(
        (codex.state, codex.other.state),
        (None, None),
        "a reset takes both"
    );
}

/// THE SHELL'S PROMPT ENDS ANOTHER PROGRAM'S WORD: a program that names itself (`app=kimi-code`)
/// and exits without a `clear` leaves its report standing only until its shell draws a prompt (an
/// OSC 133 or 633 `A`), which takes it away and moves its sequence; that prompt leaves the known
/// programs' report as it was. NEGATIVE CONTROL: the command's own `C` and output take nothing
/// away.
#[test]
fn a_shell_prompt_ends_another_programs_word() {
    use crate::terminal::Terminal;
    for prompt in ["\x1b]133;A\x07", "\x1b]633;A\x07"] {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b]7501;state=working:app=codex\x1b\\");
        term.process(b"\x1b]133;A\x07$ kimi\x1b]133;B\x07\r\n\x1b]133;C\x07");
        term.process(b"\x1b]7501;state=working:app=kimi-code\x1b\\thinking\r\n");
        let running = term.program_state();
        assert!(running.other.state.is_some(), "CONTROL: {prompt:?}");
        term.process(b"\x1b]133;D;0\x07");
        term.process(prompt.as_bytes());
        let after = term.program_state();
        assert_eq!(after.other.state, None, "{prompt:?}");
        assert!(after.other.seq > running.other.seq, "{prompt:?}");
        assert_eq!(
            (after.seq, after.state),
            (running.seq, running.state),
            "{prompt:?}: the known report stands"
        );
    }
}

/// A WORD PAST ITS BOUND IS NO REPORT: an `app=` word of 33 bytes, an empty one, and one with a
/// byte outside `[a-z0-9._-]` drop the whole report, leaving the one standing as it was.
/// NEGATIVE CONTROL: the 32-byte word is taken.
#[test]
fn an_app_word_past_the_bound_drops_the_report() {
    let past = "a".repeat(AppWord::MAX + 1);
    let mut report = ProgramStateRecord::default();
    assert!(report.accept("state=working:app=codex"));
    let standing = report;
    for word in [
        past.as_str(),
        "",
        "Kimi",
        "kimi code",
        "kimi/code",
        "kimi=code",
        "ki\u{e9}",
    ] {
        assert_eq!(AppWord::new(word), None, "{word:?}");
        let body = format!("state=idle:app={word}");
        assert_eq!(parse(&body), None, "{body:?}");
        assert!(!report.accept(&body), "{body:?}");
        assert_eq!(report, standing, "{body:?}");
    }
    let at_bound = "a".repeat(AppWord::MAX);
    assert!(
        report.accept(&format!("state=idle:app={at_bound}")),
        "CONTROL: the word at its bound is taken"
    );
}

#[test]
fn an_unknown_report_and_prompt_preserve_the_numbered_turn_cursor() {
    let mut record = ProgramStateRecord::default();
    assert!(record.accept_at("state=working:app=codex", Some(10)));
    assert!(record.accept_at("state=done:app=codex", Some(20)));
    let cursor = record.turns().head(record.seq);
    let before = record.turns().iter().collect::<Vec<_>>();
    let known = (record.seq, record.began, record.state, record.turns);

    assert!(record.accept_at("state=working:app=widget", Some(30)));
    assert!(record.other.state.is_some());
    assert_eq!(
        (record.seq, record.began, record.state, record.turns),
        known
    );
    assert_eq!(record.turns().iter().collect::<Vec<_>>(), before);
    record.prompt_drawn();
    assert!(record.other.state.is_none());
    assert_eq!(
        (record.seq, record.began, record.state, record.turns),
        known
    );
    assert_eq!(record.turns().iter().collect::<Vec<_>>(), before);

    assert!(record.accept_at("state=working:app=codex", Some(40)));
    assert!(record.accept_at("state=done:app=codex", Some(50)));
    let mut unread = Vec::new();
    assert_eq!(record.turns().after(Some(cursor), &mut unread), 0);
    assert_eq!(unread.len(), 1);
    assert_eq!(unread[0].n, cursor.n + 1);
    assert_eq!(unread[0].began.unwrap().seq, known.0 + 1);
    assert_eq!(unread[0].ended.unwrap().1.seq, known.0 + 2);
}
