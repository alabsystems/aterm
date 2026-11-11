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
        "state=idle:app=claude",
        "state=working:app=grok",
        "state=idle:app=codex:state=working",
        "state=blocked:app=codex:kind=coffee",
        "app=codex",
        "garbage",
    ] {
        assert_eq!(parse(body), None, "{body:?}");
        let mut report = ProgramStateReport::default();
        assert!(!report.accept(body), "{body:?}");
        assert_eq!(report, ProgramStateReport::default(), "{body:?}");
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
    let mut report = ProgramStateReport::default();
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
    let mut report = ProgramStateReport::default();
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
    let mut live = ProgramStateReport::default();
    assert!(live.accept("state=blocked:app=codex:kind=permission"));
    assert_eq!(live.turns, 1, "CONTROL: a first report begins a turn");
    let body = live.carried();
    assert_eq!(
        body.as_deref(),
        Some("state=blocked:app=codex:kind=permission")
    );
    let mut restored = ProgramStateReport::default();
    restored.restore(body.as_deref());
    assert_eq!(restored.state, live.state);
    assert_eq!((restored.seq, restored.began, restored.turns), (1, 1, 0));
    assert!(restored.accept("state=blocked:app=codex:kind=permission"));
    assert_eq!(
        (restored.began, restored.turns),
        (1, 0),
        "a re-send of the carried box is the same box"
    );
    let numbering = (restored.seq, restored.began, restored.turns);
    restored.restore(body.as_deref());
    assert_eq!((restored.seq, restored.began, restored.turns), numbering);
    restored.restore(body.as_deref());
    assert_eq!((restored.seq, restored.began, restored.turns), numbering);
    assert!(restored.accept("state=working:app=codex"));
    assert_eq!(restored.turns, 0, "the carried turn goes on");
    for body in [None, Some("state=clear"), Some("state=sleeping:app=codex")] {
        let mut none = ProgramStateReport::default();
        none.restore(body);
        assert_eq!(none, ProgramStateReport::default(), "{body:?}");
    }
    let mut idle = ProgramStateReport::default();
    assert!(idle.accept("state=idle:app=codex"));
    assert_eq!(idle.carried().as_deref(), Some("state=idle:app=codex"));
    idle.restore(None);
    assert_eq!((idle.seq, idle.began, idle.state), (2, 2, None));
}

/// What one report does to the turn edges ([`ProgramStateReport::turn_edges`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Edge {
    /// Adds a turn it began.
    Begins,
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
        Edge::Neither,
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
    let mut report = ProgramStateReport::default();
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
    assert!(!report.accept("state=idle:app=claude"));
    assert!(!report.accept("state=working:app=claude"));
    assert_eq!(report.turns, 6);
}

/// EACH TURN KEEPS THE BATCH TIMES OF ITS EDGES: a turn begins at its first `working`, dated
/// at the wall clock of the batch it arrived in, and ends at the next rest, dated at that
/// rest's batch and kept with its word. NEGATIVE CONTROLS: the re-send at 1005 and the box
/// asked and answered at 1010 and 1012, none of which begins or ends a turn.
#[test]
fn each_turn_keeps_the_batch_times_of_its_edges() {
    use ProgramStateWord::*;
    let edge = |seq, wall_ms| TurnEdge {
        seq,
        wall_ms: Some(wall_ms),
    };
    let turn = |began, ended| ProgramTurn {
        app: ProgramApp::Codex,
        began: Some(began),
        ended,
    };
    let mut report = ProgramStateReport::default();
    for (body, wall_ms) in [
        ("state=working:app=codex", 1_000),
        ("state=working:app=codex", 1_005),
        ("state=blocked:app=codex:kind=permission", 1_010),
        ("state=working:app=codex", 1_012),
    ] {
        assert!(report.accept_at(body, Some(wall_ms)), "{body}");
    }
    assert_eq!(
        report.turn_edges().collect::<Vec<_>>(),
        [turn(edge(1, 1_000), None)],
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
        report.turn_edges().collect::<Vec<_>>(),
        [
            turn(edge(1, 1_000), Some((Done, edge(5, 1_017)))),
            turn(edge(6, 2_000), Some((Error, edge(7, 2_030)))),
        ]
    );
}

/// THE EDGES FOLLOW THE TURN COUNT: over the turn count's reports, each in a batch of its own
/// time, a turn is added with its edge exactly where the count moves, and the rows that end a
/// turn end the newest: the turn resumed across a `clear` ends on its own entry, the `clear`
/// having ended nothing. After a full reset a resumed turn ends on its own entry too, one
/// begun across a reset is added, and a report this reader does not take changes nothing; the
/// newest four are kept. NEGATIVE CONTROLS: every `Neither` row, `clear`s among them.
#[test]
fn the_edges_follow_the_turn_count() {
    fn step(
        report: &mut ProgramStateReport,
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
            "a turn is added where the count moves: {why}: {body}"
        );
        let edge = TurnEdge {
            seq: report.seq,
            wall_ms: Some(wall_ms),
        };
        match moves {
            Edge::Begins => want.push(ProgramTurn {
                app: ProgramApp::Codex,
                began: Some(edge),
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
        let kept = want.len().saturating_sub(TURN_EDGES_KEPT);
        assert_eq!(
            report.turn_edges().collect::<Vec<_>>(),
            want[kept..],
            "{why}: {body}"
        );
    }
    let mut report = ProgramStateReport::default();
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
            Edge::Neither,
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
    assert_eq!(want.len(), 6, "six turns ran");
    let kept: Vec<_> = report.turn_edges().collect();
    assert!(!report.accept_at("state=idle:app=claude", Some(wall_ms + 10)));
    assert_eq!(report.turn_edges().collect::<Vec<_>>(), kept);
}

/// THE NEWEST FOUR ARE KEPT, AND A RESTORED TURN ENDS UNBEGUN: of six turns the newest four
/// are kept, oldest first. Restoring a different report clears them, and the turn it
/// installed running ends on an entry with no beginning that begins no turn. NEGATIVE
/// CONTROL: restoring the report standing keeps them.
#[test]
fn the_newest_four_are_kept_and_a_restored_turn_ends_unbegun() {
    let mut report = ProgramStateReport::default();
    for turn in 0..6_u64 {
        assert!(report.accept_at("state=working:app=codex", Some(1_000 * turn)));
        assert!(report.accept_at("state=done:app=codex", Some(1_000 * turn + 17)));
    }
    let began: Vec<_> = report
        .turn_edges()
        .map(|turn| turn.began.and_then(|edge| edge.wall_ms))
        .collect();
    assert_eq!(
        began,
        [Some(2_000), Some(3_000), Some(4_000), Some(5_000)],
        "the newest four, oldest first"
    );
    let kept: Vec<_> = report.turn_edges().collect();
    let body = report.carried();
    report.restore(body.as_deref());
    assert_eq!(
        report.turn_edges().collect::<Vec<_>>(),
        kept,
        "CONTROL: the report standing restored keeps them"
    );
    report.restore(Some("state=working:app=codex"));
    assert_eq!(
        report.turn_edges().count(),
        0,
        "a different report restored clears them"
    );
    assert!(report.accept_at("state=done:app=codex", Some(7_017)));
    assert_eq!(
        report.turn_edges().collect::<Vec<_>>(),
        [ProgramTurn {
            app: ProgramApp::Codex,
            began: None,
            ended: Some((
                ProgramStateWord::Done,
                TurnEdge {
                    seq: report.seq,
                    wall_ms: Some(7_017),
                },
            )),
        }],
        "the restored turn ends on an entry with no beginning"
    );
    assert_eq!(report.turns, 6, "and begins none");
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
    let mut live = ProgramStateReport::default();
    assert!(live.accept("state=blocked:kind=permission:app=grok-build:title=cGFyc2Vy"));
    let body = live.carried();
    assert_eq!(
        body.as_deref(),
        Some("state=blocked:app=grok-build:kind=permission")
    );
    let mut restored = ProgramStateReport::default();
    restored.restore(body.as_deref());
    assert_eq!(restored.state, live.state);
    assert!(restored.accept("state=blocked:kind=permission:app=grok-build"));
    assert_eq!((restored.began, restored.turns), (1, 0), "the same box");
    let mut codex = ProgramStateReport::default();
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
