// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// THE TASK'S FACE in the loop, over the scripted server
// (`docs/DESIGN-harness-autopilot-2026-10-15.md` §9.4.2, §9.4.5): what a
// recording host is told through `IdleHost::task_face` — once at the loop's
// start, then where a point moves it (landed, done, theirs, spent) and at the
// first live read of someone else's turn — and the landing a loop before
// this one landed, read back from the journal beside the ledger
// (`ended_before`, a done likewise, its time kept), so a restart types no
// land instruction into finished work, and never takes someone else's later
// done turn for the landing. Each
// behaviour has its negative control. Included from `run_tests.rs` (`mod
// task_face_loop`), so the `Mock` and its helpers are in scope.

use crate::harness::acts::TaskFace;
use crate::supervise::policy::turn_end::{RULE_DONE_CHECK, RULE_LAND, RULE_LEFT};

/// The worker's own token, as Land's instruction asks for it.
const TOKEN_REPLY: &str = "Committed and pushed the parser and its tests.\nATERM-TASK-DONE";
/// A reply that says no done.
const PROGRESS: &str = "Committing the parser now; the lexer is next.";

/// A host that records every face it is told, and how it started the loop
/// ([`IdleHost::lifted_off`]).
#[derive(Debug, Default)]
struct Faces {
    told: Mutex<Vec<TaskFace>>,
    /// The faces told restated from the journal, with their note's time
    /// ([`IdleHost::task_face_noted`]); each is in `told` too.
    noted: Mutex<Vec<(TaskFace, i64)>>,
    lifted: bool,
}

impl Faces {
    fn told(&self) -> Vec<TaskFace> {
        self.told
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn noted(&self) -> Vec<(TaskFace, i64)> {
        self.noted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl IdleHost for Faces {
    fn wants(&self) -> bool {
        false
    }
    fn at_idle(&self) -> Option<HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn lifted_off(&self) -> bool {
        self.lifted
    }
    fn task_face(&self, face: TaskFace) {
        self.told
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(face);
    }
    fn task_face_noted(&self, face: TaskFace, noted_ms: i64) {
        self.noted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((face, noted_ms));
        self.task_face(face);
    }
}

/// A turn that ended after 3 minutes of work (the done row's measure), the
/// worker's last words `said` (each line its own row), at an empty composer.
fn ended(said: &str) -> Vec<String> {
    let mut lines: Vec<String> = said.lines().map(|l| format!("⏺ {l}")).collect();
    for l in lines.iter_mut().skip(1) {
        *l = l.replacen("⏺ ", "  ", 1);
    }
    lines.extend([
        String::new(),
        "✻ Worked for 3m 2s · done 4:24 PM".to_string(),
        String::new(),
    ]);
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    let mut r = rows(&refs);
    r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
    r
}

/// A run over `screens` with the session's word `word` and `host`, at test
/// speed (a 30 ms back-off): the lines said and the turns typed. `ledger`:
/// the approval ledger the loop keeps, its journal beside it.
fn run(
    screens: Vec<Vec<String>>,
    word: &'static str,
    gates: Vec<usize>,
    host: &Arc<Faces>,
    ledger: Option<&Path>,
    human: Option<&'static str>,
) -> (Vec<String>, usize) {
    run_on(screens, word, gates, host, ledger, human, |_| {})
}

/// [`run`], the mock set up further by `more` (a word the owner changes
/// while the loop runs, say).
fn run_on(
    screens: Vec<Vec<String>>,
    word: &'static str,
    gates: Vec<usize>,
    host: &Arc<Faces>,
    ledger: Option<&Path>,
    human: Option<&'static str>,
    more: impl FnOnce(&mut Mock),
) -> (Vec<String>, usize) {
    let (lines, turns, _) = run_kept(screens, word, gates, host, ledger, human, more);
    (lines, turns)
}

/// [`run_on`], the mock handed back as the run left it (the word it ended
/// on, how often the loop read it).
fn run_kept(
    screens: Vec<Vec<String>>,
    word: &'static str,
    gates: Vec<usize>,
    host: &Arc<Faces>,
    ledger: Option<&Path>,
    human: Option<&'static str>,
    more: impl FnOnce(&mut Mock),
) -> (Vec<String>, usize, Mock) {
    let mut m = Mock::new(screens);
    m.autopilot = Some(word);
    m.turn_gates = gates;
    m.vanish_after = Some(6);
    m.stall_sleep = Some(Duration::from_millis(20));
    m.human = human.into_iter().collect();
    more(&mut m);
    let opts = SuperviseOpts {
        policy: SupervisorConfig {
            human_grace_s: 0,
            ..SupervisorConfig::default()
        },
        journal: ledger.map(crate::supervise::approvals::journal_beside),
        idle_host: Some(Arc::clone(host) as Arc<dyn IdleHost>),
        ..auto(30)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            short_backoff: Duration::from_millis(30),
            short_backoff_max: Duration::from_millis(60),
            ..TurnEndTiming::default()
        });
        s.set_approval_ledger(ledger.map(Path::to_path_buf));
    });
    let turns = count(&m, "turn");
    (lines, turns, m)
}

/// LAND, ITS DONE REPLY: told `open` once at the loop's start, `landed`
/// exactly once at the point the journal says `DONE … rule=land@v1`, and
/// nothing more while that point stands. Then SOMEONE ELSE'S TURN, read busy
/// with no act of the policy's awaited: `open` at its first live read, before
/// any point of it. NEGATIVE CONTROL: the same run with that turn never begun
/// stays landed.
#[test]
fn a_landing_is_told_landed_once_and_someone_elses_live_turn_opens_it() {
    let landed = |then: Vec<Vec<String>>| {
        let host = Arc::new(Faces::default());
        let mut screens = vec![
            busy_screen(),
            ended(PROGRESS),
            busy_screen(),
            ended(TOKEN_REPLY),
        ];
        screens.extend(then);
        let (lines, turns) = run(screens, "land", vec![1], &host, None, None);
        assert_eq!(turns, 1, "the instruction alone: {lines:#?}");
        assert!(
            lines
                .iter()
                .any(|l| l.contains(&format!("rule={RULE_LAND} "))),
            "{lines:#?}"
        );
        host.told()
    };
    assert_eq!(
        landed(vec![busy_screen()]),
        [TaskFace::Open, TaskFace::Landed, TaskFace::Open]
    );
    // NEGATIVE CONTROL: nobody's turn after it.
    assert_eq!(landed(Vec::new()), [TaskFace::Open, TaskFace::Landed]);
}

/// CRUISE ENDS ARE `done`, NEVER `landed`: the worker's own token after a
/// continuation, and the done check's second done report, are told `done`
/// once. A person's quick greeting under Land is told `theirs` (the journal's
/// `LEFT`), never `landed` or `done`.
#[test]
fn a_done_check_or_finished_end_is_done_and_a_persons_exchange_is_theirs() {
    for (last, check) in [
        (TOKEN_REPLY, false),
        ("All done. Nothing left to do.", true),
    ] {
        let host = Arc::new(Faces::default());
        let mut screens = vec![busy_screen(), ended(PROGRESS), busy_screen(), ended(last)];
        let mut gates = vec![1];
        if check {
            // The done report gets the check, and the check's reply says
            // done again.
            screens.extend([busy_screen(), ended(last)]);
            gates.push(3);
        }
        let (lines, turns) = run(screens, "cruise", gates, &host, None, None);
        assert_eq!(turns, 1 + usize::from(check), "{lines:#?}");
        if check {
            assert!(
                lines
                    .iter()
                    .any(|l| l.contains(&format!("rule={RULE_DONE_CHECK} "))),
                "{lines:#?}"
            );
        }
        assert_eq!(host.told(), [TaskFace::Open, TaskFace::Done], "{last}");
    }
    // A person's greeting under Land: theirs.
    let host = Arc::new(Faces::default());
    let mut quick = rows(&["⏺ Hi! What would you like me to work on?", ""]);
    quick.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
    let (lines, turns) = run(
        vec![busy_screen(), quick],
        "land",
        Vec::new(),
        &host,
        None,
        Some("2000"),
    );
    assert_eq!(turns, 0, "{lines:#?}");
    assert_eq!(host.told(), [TaskFace::Open, TaskFace::Theirs]);
    let _ = RULE_LEFT;
}

/// A LANDING SPENT: the instruction and both nudges answered with work and
/// no done — told `spent` once, after the second nudge's turn ended, and
/// nothing new at someone else's live turn after it. NEGATIVE CONTROL: the
/// second nudge's done reply is told `landed` — and someone else's live turn
/// after THAT reads open, never spent: the agent did reply done, and spent's
/// words (the chip's title, the window's record) say it did not.
#[test]
fn a_spent_landing_is_told_once_and_someone_elses_turn_after_it_moves_nothing() {
    let spend = |last: &str| {
        let host = Arc::new(Faces::default());
        let screens = vec![
            busy_screen(),
            ended(PROGRESS),
            busy_screen(),
            ended(PROGRESS),
            busy_screen(),
            ended(PROGRESS),
            busy_screen(),
            ended(last),
            busy_screen(),
        ];
        let (lines, turns) = run(screens, "land", vec![1, 3, 5], &host, None, None);
        assert_eq!(turns, 3, "the instruction and two nudges: {lines:#?}");
        host.told()
    };
    assert_eq!(spend(PROGRESS), [TaskFace::Open, TaskFace::Spent]);
    // NEGATIVE CONTROL: the second nudge's done reply lands it; someone
    // else's turn after that is open, as after any landing.
    assert_eq!(
        spend(TOKEN_REPLY),
        [TaskFace::Open, TaskFace::Landed, TaskFace::Open]
    );
}

/// A RESTART OF A LANDING SPENT TELLS `spent` AT ITS START and types nothing
/// (the ledger's land episode, `approvals::LandSeed::quiet`): the loop that
/// spent it wrote its acts and points; a loop that follows on its ledger is
/// told the face that loop held, once, and nothing after. Unseeded, it was
/// told `open` over an episode it types nothing more of. NEGATIVE CONTROL:
/// the same restart after the last nudge's done reply is told `landed`.
#[test]
fn a_restart_of_a_spent_landing_tells_spent_and_types_nothing() {
    for (last, face) in [(PROGRESS, TaskFace::Spent), (TOKEN_REPLY, TaskFace::Landed)] {
        let (dir, path) = files("restart-spent", &[], &[]);
        let host = Arc::new(Faces::default());
        let screens = vec![
            busy_screen(),
            ended(PROGRESS),
            busy_screen(),
            ended(PROGRESS),
            busy_screen(),
            ended(PROGRESS),
            busy_screen(),
            ended(last),
        ];
        let (lines, turns) = run(screens, "land", vec![1, 3, 5], &host, Some(&path), None);
        assert_eq!(turns, 3, "the instruction and two nudges: {lines:#?}");
        let host = Arc::new(Faces::default());
        let (lines, turns) = run(
            vec![ended(last)],
            "land",
            Vec::new(),
            &host,
            Some(&path),
            None,
        );
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(turns, 0, "{last}: {lines:#?}");
        assert_eq!(host.told(), [face], "{last}: {lines:#?}");
    }
}

/// THE LOOP'S START TELLS ITS FACE ONCE, whatever it is: an idle session
/// with nothing to end is told `open` and nothing more. With no host (a CLI
/// `watch`), nothing is told and nothing fails.
#[test]
fn the_loops_start_is_told_once() {
    let host = Arc::new(Faces::default());
    let (_, turns) = run(vec![ended(PROGRESS)], "off", Vec::new(), &host, None, None);
    assert_eq!(turns, 0);
    assert_eq!(host.told(), [TaskFace::Open]);
}

/// A ledger row as the loop writes it, `ago` before now.
fn ledger_row(rule_id: &str, command: &str, ago: Duration) -> String {
    let row = crate::supervise::approvals::Row {
        rule_id,
        outcome: crate::supervise::approvals::Outcome::Typed,
        command,
        reason: "-",
        box_seq: 1,
    };
    let at = crate::supervise::journal::unix_ms() - i64::try_from(ago.as_millis()).unwrap();
    row.to_json(at, None)
}

/// A journal record of `line` as the loop writes it, `ago` before now.
fn journal_row(line: &str, ago: Duration, sid: Option<&str>) -> String {
    marked_row(line, ago, sid, None)
}

/// [`journal_row`] of a review point, the worker's last words there marked
/// `mark` ([`said_mark`]).
fn marked_row(line: &str, ago: Duration, sid: Option<&str>, mark: Option<u64>) -> String {
    let at = crate::supervise::journal::unix_ms() - i64::try_from(ago.as_millis()).unwrap();
    let mut rec = crate::supervise::journal::JournalRecord::of_line(at, sid, line, None);
    rec.mark = mark;
    rec.to_json()
}

const LANDED_NOTE: &str = "DONE seq=104 rule=land@v1 the worker said it is done after the land \
     instruction (autopilot land): nothing more is typed until a person or an orchestrator types";

/// A ledger and the journal beside it, holding `ledger` and `journal` rows.
fn files(tag: &str, ledger: &[String], journal: &[String]) -> (PathBuf, PathBuf) {
    let dir = crate::supervise::test_scratch_path("task-face", tag);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("tmp dir");
    let path = dir.join("s-land.jsonl");
    let body = |rows: &[String]| rows.iter().map(|r| format!("{r}\n")).collect::<String>();
    std::fs::write(&path, body(ledger)).expect("ledger");
    std::fs::write(
        crate::supervise::approvals::journal_beside(&path),
        body(journal),
    )
    .expect("journal");
    (dir, path)
}

/// A later box disproves a legacy landing even if a successor reuses its seq.
/// Fixed journal buffers exercise the actual reader without clocks or files.
#[test]
fn an_old_landing_never_survives_a_later_box_with_the_same_sequence() {
    const POINT: &str = r#"{"t":1,"sid":null,"kind":"event","phase":"idle","seq":104,"complete":null,"rows":null,"summary":"ATERM-TASK-DONE","line":"EVENT idle seq=104 ATERM-TASK-DONE","turn":null,"report":null}"#;
    const NOTE: &str = r#"{"t":2,"sid":null,"kind":"other","phase":"-","seq":null,"complete":null,"rows":null,"summary":"DONE seq=104 rule=land@v1 the worker said it is done after the land instruction","line":"DONE seq=104 rule=land@v1 the worker said it is done after the land instruction","turn":null,"report":null}"#;
    const AGAIN: &str = r#"{"t":3,"sid":null,"kind":"event","phase":"idle","seq":104,"complete":null,"rows":null,"summary":"ATERM-TASK-DONE","line":"EVENT idle seq=104 ATERM-TASK-DONE","turn":null,"report":null}"#;
    const MARKED_POINT: &str = r#"{"t":1,"sid":null,"kind":"event","phase":"idle","seq":104,"complete":null,"rows":null,"summary":"ATERM-TASK-DONE","line":"EVENT idle seq=104 ATERM-TASK-DONE","turn":null,"report":null,"mark":"feed00000000beef"}"#;
    const MARKED_AGAIN: &str = r#"{"t":3,"sid":null,"kind":"event","phase":"idle","seq":9,"complete":null,"rows":null,"summary":"ATERM-TASK-DONE","line":"EVENT idle seq=9 ATERM-TASK-DONE","turn":null,"report":null,"mark":"feed00000000beef"}"#;
    const PROMPT: &str = r#"{"t":3,"sid":null,"kind":"event","phase":"prompt","seq":104,"complete":null,"rows":null,"summary":"kind=bash classify=- command=touch c","line":"EVENT prompt seq=104 kind=bash classify=- command=touch c","turn":null,"report":null}"#;
    const APPROVED: &str = r#"{"t":3,"sid":null,"kind":"approved","phase":"prompt","seq":104,"complete":null,"rows":null,"summary":"touch c.txt","line":"APPROVED seq=104 touch c.txt","turn":null,"report":null}"#;

    // Ordinary old journals still seed, with or without a recorded point;
    // an ordinary point on the same seq preserves that compatibility.
    for prefix in [NOTE.to_string(), format!("{POINT}\n{NOTE}")] {
        assert_eq!(landed_before(&prefix, None, Some(1)), Some(None));
        assert_eq!(
            landed_before(&format!("{prefix}\n{AGAIN}"), None, Some(1)),
            Some(None)
        );
    }
    // A marked landing also retains its ordinary redraw at a different seq.
    let mark = crate::supervise::journal::JournalRecord::from_json(MARKED_POINT)
        .expect("the captured marked point")
        .mark;
    assert!(mark.is_some());
    assert_eq!(
        landed_before(
            &format!("{MARKED_POINT}\n{NOTE}\n{MARKED_AGAIN}"),
            None,
            Some(1)
        ),
        Some(mark)
    );
    for after in [PROMPT, APPROVED] {
        assert_eq!(
            landed_before(&format!("{MARKED_POINT}\n{NOTE}\n{after}"), None, Some(1)),
            None,
            "a marked landing also refuses an explicit later box"
        );
    }
    // Collect all four legacy failures before the final assertion: no point
    // or an unmarked point before the note, then either actual box syntax.
    let mut wrong = Vec::new();
    for (history, prefix) in [
        ("no prior point", NOTE.to_string()),
        ("unmarked prior point", format!("{POINT}\n{NOTE}")),
    ] {
        for (kind, after) in [("prompt", PROMPT), ("approved", APPROVED)] {
            let got = landed_before(&format!("{prefix}\n{after}"), None, Some(1));
            if got.is_some() {
                wrong.push(format!("{history}, {kind}: {got:?}"));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "legacy landing seeds must reject explicit later boxes at the same sequence: {wrong:#?}"
    );
}

/// THE LANDING READ BACK (`landed_before`): the last note of a task's end is
/// the land's `DONE`, and no typed row came after it. Not where a person's
/// `LEFT` or another end (`rule=finished@v1`, the done check's) came after
/// it, not where a typed row did, not for another session's note, and not
/// with no journal at all. A note with no marked point before it (a journal
/// a build before the mark wrote) reads its later points by `seq=` and seeds
/// no words; the marked case is the test after this one.
#[test]
fn the_landing_is_read_back_from_the_journal_beside_the_ledger() {
    let min = |n: u64| Duration::from_secs(60 * n);
    let seeded = |tag: &str, ledger: &[String], journal: &[String]| {
        let (dir, path) = files(tag, ledger, journal);
        let typed = crate::supervise::approvals::typed_at(&path, None);
        let journal = std::fs::read_to_string(crate::supervise::approvals::journal_beside(&path))
            .unwrap_or_default();
        let got = landed_before(&journal, None, typed.last().copied());
        let _ = std::fs::remove_dir_all(&dir);
        got.map(|words| assert_eq!(words, None, "{tag}: no mark, no words"))
            .is_some()
    };
    let land_typed = ledger_row(RULE_LAND, "land", min(10));
    let note = journal_row(LANDED_NOTE, min(9), None);
    assert!(seeded(
        "read-back",
        std::slice::from_ref(&land_typed),
        std::slice::from_ref(&note)
    ));
    // A note after it, of a task's end other than the land's.
    for later in [
        "LEFT seq=110 rule=left-to-person@v1 a person's own exchange (their message, answered \
         within 10 min): nothing is typed until the next turn ends",
        "DONE seq=110 rule=finished@v1 the worker reported ATERM-TASK-DONE: its task is finished; \
         nothing more is typed until a person or an orchestrator types",
        "DONE seq=110 rule=continue-done-check@v1 the worker said it is done after the done \
         check: nothing more is typed until a person or an orchestrator types",
    ] {
        let after = journal_row(later, min(5), None);
        assert!(
            !seeded(
                "read-back-later",
                std::slice::from_ref(&land_typed),
                &[note.clone(), after]
            ),
            "{later}"
        );
    }
    // Lines after it that say no point of a turn: what an idle screen shows
    // with no turn, what the loop says of itself, and the landing's own
    // point said again (its seq).
    for other in [
        "EVENT context seq=111 9% until auto-compact",
        "EVENT survey seq=111 dismiss with: aterm ctl @s-1 key 'if=^●.How.is.Claude.doing' 0",
        "DISMISSED survey seq=111",
        "RECONNECTED after 900 ms",
        "EXIT stopped",
        "EVENT idle seq=104 ⏺ Done.",
    ] {
        let after = journal_row(other, min(5), None);
        assert!(
            seeded(
                "read-back-other",
                std::slice::from_ref(&land_typed),
                &[note.clone(), after]
            ),
            "{other}"
        );
    }
    // A LATER POINT OR BOX: someone else's turn after the landing, its end
    // handed over or a box of it approved — though nothing was typed and no
    // end was noted (the Together run: a person's turn ending on the token).
    // This read seeded the landing before the fix, which is how that reply
    // was journaled "after the land instruction".
    for later in [
        "EVENT idle seq=111 ⏺ Done.",
        "EVENT idle seq=111 ATERM-TASK-DONE",
        "EVENT question seq=111 ⏺ Keep the harness or rewrite it?",
        "EVENT prompt seq=109 kind=bash classify=not-read-only:rm command=rm -rf x",
        "EVENT turn seq=111 report=5 rows=12 ⏺ Done.",
        "APPROVED seq=108 calc.py",
    ] {
        let after = journal_row(later, min(5), None);
        assert!(
            !seeded(
                "read-back-point",
                std::slice::from_ref(&land_typed),
                &[note.clone(), after]
            ),
            "{later}"
        );
    }
    // …and another session's point is not this one's.
    let elsewhere = journal_row("EVENT idle seq=111 ⏺ Done.", min(5), Some("s-other"));
    assert!(seeded(
        "read-back-point-sid",
        std::slice::from_ref(&land_typed),
        &[note.clone(), elsewhere]
    ));
    // A typed row after it.
    let nudge = ledger_row("continue@v1", "keep going", min(2));
    assert!(!seeded(
        "read-back-typed",
        &[land_typed.clone(), nudge],
        std::slice::from_ref(&note)
    ));
    // Another session's note.
    let theirs = journal_row(LANDED_NOTE, min(9), Some("s-other"));
    assert!(!seeded(
        "read-back-sid",
        std::slice::from_ref(&land_typed),
        &[theirs]
    ));
    // No note, and no journal (the door reads a missing one as empty).
    assert!(!seeded("read-back-none", &[land_typed], &[]));
    assert!(landed_before("", None, None).is_none());
}

/// THE LANDING'S POINT IS READ BY ITS WORDS, NEVER ITS SEQ (the review of
/// Together D3, 2026-10-17): the journal keeps the mark of the worker's last
/// words with each point ([`said_mark`]), and the landing's mark is the one
/// on the point before its note. A later point with the same mark is the
/// landing's own — said again after an outage under the successor's
/// restarted seq (`RECONNECTED`, then `EVENT idle seq=9`), or after a
/// resize — and the seed holds, carrying that mark for the first point to
/// compare. Another reply's mark, a point that marks none and any box drop
/// it. NEGATIVE CONTROL: the same outage's point read by its seq (a journal
/// with no marks, the test above) drops the seed, which typed the land
/// instruction into finished work after the next restart.
#[test]
fn a_landings_point_said_again_is_read_by_its_words_never_its_seq() {
    let min = |n: u64| Duration::from_secs(60 * n);
    let landing = said_mark(Some(
        "Committed and pushed the parser and its tests.\nATERM-TASK-DONE",
    ));
    let theirs = said_mark(Some(
        "Added the module docstring and committed it.\nATERM-TASK-DONE",
    ));
    let read = |tag: &str, after: &[String]| {
        let land_typed = ledger_row(RULE_LAND, "land", min(10));
        let mut journal = vec![
            marked_row("EVENT idle seq=104 ATERM-TASK-DONE", min(9), None, landing),
            marked_row(
                "EVENT context seq=104 9% until auto-compact",
                min(9),
                None,
                None,
            ),
            journal_row(LANDED_NOTE, min(9), None),
        ];
        journal.extend_from_slice(after);
        let (dir, path) = files(tag, std::slice::from_ref(&land_typed), &journal);
        let typed = crate::supervise::approvals::typed_at(&path, None);
        let journal = std::fs::read_to_string(crate::supervise::approvals::journal_beside(&path))
            .unwrap_or_default();
        let got = landed_before(&journal, None, typed.last().copied());
        let _ = std::fs::remove_dir_all(&dir);
        got
    };
    assert_eq!(
        read("words-plain", &[]),
        Some(landing),
        "the landing's mark goes with the seed"
    );
    let reconnected = journal_row("RECONNECTED after 501 ms", min(8), None);
    let again = marked_row("EVENT idle seq=9 ATERM-TASK-DONE", min(8), None, landing);
    assert_eq!(
        read("words-again", &[reconnected.clone(), again.clone()]),
        Some(landing),
        "the landing's point said again under another seq"
    );
    for (tag, later) in [
        (
            "words-theirs",
            marked_row("EVENT idle seq=9 ATERM-TASK-DONE", min(7), None, theirs),
        ),
        (
            "words-none",
            journal_row("EVENT idle seq=9 ATERM-TASK-DONE", min(7), None),
        ),
        (
            "words-box",
            journal_row(
                "EVENT prompt seq=9 kind=bash classify=- command=touch c",
                min(7),
                None,
            ),
        ),
        (
            "words-approved",
            journal_row("APPROVED seq=9 touch c.txt", min(7), None),
        ),
    ] {
        assert_eq!(
            read(tag, &[reconnected.clone(), again.clone(), later]),
            None,
            "{tag}"
        );
    }
    // NEGATIVE CONTROL: the outage's point with no mark (read by its seq).
    let unmarked = journal_row("EVENT idle seq=9 ATERM-TASK-DONE", min(8), None);
    let (dir, path) = files(
        "words-by-seq",
        &[ledger_row(RULE_LAND, "land", min(10))],
        &[
            journal_row("EVENT idle seq=104 ATERM-TASK-DONE", min(9), None),
            journal_row(LANDED_NOTE, min(9), None),
            reconnected,
            unmarked,
        ],
    );
    let typed = crate::supervise::approvals::typed_at(&path, None);
    let journal = std::fs::read_to_string(crate::supervise::approvals::journal_beside(&path))
        .unwrap_or_default();
    let got = landed_before(&journal, None, typed.last().copied());
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(got, None, "by its seq the point is another one");
}

/// A DONE OF ANY KIND IS READ BACK AS DONE, A LANDING AS LANDED
/// (`ended_before`, the done's seed of §10.2.7): the last note of a task's
/// end is the worker's `FINISHED` (`rule=finished@v1`), the done check's or
/// the open-items challenge's `DONE`, and nothing typed or judged came after
/// it — a done, its point's mark with it; the land's `DONE` is a landing,
/// and the last of the two notes is the one read. NEGATIVE CONTROLS: a
/// person's `LEFT` as the last end, a typed row after the note, another
/// reply's point after it, and another session's note each seed nothing.
#[test]
fn a_done_of_any_kind_is_read_back_as_done_and_a_landing_as_landed() {
    let min = |n: u64| Duration::from_secs(60 * n);
    let mark = said_mark(Some(TOKEN_REPLY));
    let theirs = said_mark(Some(THEIR_TOKEN_REPLY));
    assert!(mark.is_some() && mark != theirs);
    let finished = "DONE seq=104 rule=finished@v1 the worker reported ATERM-TASK-DONE: its task \
         is finished; nothing more is typed until a person or an orchestrator types";
    let check = "DONE seq=104 rule=continue-done-check@v1 the worker said it is done after the \
         done check: nothing more is typed until a person or an orchestrator types";
    let open_items = "DONE seq=104 rule=continue-open-items@v1 the worker said it is done after \
         the open-items challenge: nothing more is typed until a person or an orchestrator types";
    let left = "LEFT seq=104 rule=left-to-person@v1 a person's own exchange (their message, \
         answered within 10 min): nothing is typed until the next turn ends";
    let read =
        |tag: &str, ledger: &[String], notes: &[&str], after: &[String], sid: Option<&str>| {
            // Each note after its point, as the loop writes them.
            let mut journal: Vec<String> = notes
                .iter()
                .flat_map(|n| {
                    [
                        marked_row("EVENT idle seq=104 ATERM-TASK-DONE", min(9), sid, mark),
                        journal_row(n, min(9), sid),
                    ]
                })
                .collect();
            journal.extend_from_slice(after);
            let (dir, path) = files(tag, ledger, &journal);
            let typed = crate::supervise::approvals::typed_at(&path, None);
            let journal =
                std::fs::read_to_string(crate::supervise::approvals::journal_beside(&path))
                    .unwrap_or_default();
            let got = ended_before(&journal, None, typed.last().copied())
                .map(|(face, words, _)| (face, words));
            let _ = std::fs::remove_dir_all(&dir);
            got
        };
    let earlier = [ledger_row("continue@v1", "keep going", min(10))];
    for note in [finished, check, open_items] {
        assert_eq!(
            read("ended-done", &earlier, &[note], &[], None),
            Some((TaskFace::Done, mark)),
            "{note}"
        );
    }
    assert_eq!(
        read("ended-land", &earlier, &[LANDED_NOTE], &[], None),
        Some((TaskFace::Landed, mark))
    );
    assert_eq!(
        read(
            "ended-land-then-done",
            &earlier,
            &[LANDED_NOTE, finished],
            &[],
            None
        ),
        Some((TaskFace::Done, mark)),
        "the last end is the one read"
    );
    assert_eq!(
        read(
            "ended-done-then-land",
            &earlier,
            &[finished, LANDED_NOTE],
            &[],
            None
        ),
        Some((TaskFace::Landed, mark))
    );
    // NEGATIVE CONTROLS.
    assert_eq!(
        read("ended-left", &earlier, &[finished, left], &[], None),
        None
    );
    let typed = [
        earlier[0].clone(),
        ledger_row("continue@v1", "keep going", min(2)),
    ];
    assert_eq!(read("ended-typed", &typed, &[finished], &[], None), None);
    let point = marked_row("EVENT idle seq=9 ATERM-TASK-DONE", min(5), None, theirs);
    assert_eq!(
        read("ended-their-point", &earlier, &[finished], &[point], None),
        None
    );
    let own = marked_row("EVENT idle seq=9 ATERM-TASK-DONE", min(5), None, mark);
    assert_eq!(
        read("ended-own-point", &earlier, &[finished], &[own], None),
        Some((TaskFace::Done, mark)),
        "its own point said again keeps it"
    );
    assert_eq!(
        read("ended-sid", &earlier, &[finished], &[], Some("s-other")),
        None
    );
}

/// The first loop of a CRUISE DONE over the ledger and journal of `tag`: the
/// job's progress, a continuation, the worker's token — `DONE …
/// rule=finished@v1` — told open then done, one turn typed. The files' dir
/// and ledger.
fn finished_first(tag: &str) -> (PathBuf, PathBuf) {
    let (dir, ledger) = files(tag, &[], &[]);
    let host = Arc::new(Faces::default());
    let screens = vec![
        busy_screen(),
        ended(PROGRESS),
        busy_screen(),
        ended(TOKEN_REPLY),
    ];
    let (lines, turns) = run(screens, "cruise", vec![1], &host, Some(&ledger), None);
    assert_eq!(turns, 1, "{tag}: {lines:#?}");
    assert_eq!(host.told(), [TaskFace::Open, TaskFace::Done], "{tag}");
    assert!(
        journal_lines(&ledger)
            .iter()
            .any(|l| l.starts_with("DONE ") && l.contains(" rule=finished@v1 ")),
        "{tag}: {lines:#?}"
    );
    (dir, ledger)
}

/// A RESTARTED DONE IS RESTATED, ITS TIME KEPT (the review of 2026-10-17,
/// owner-eye-4: the done clock was stamped again at a supervision restart):
/// a Cruise task done, then its loop restarted — in place, or as the
/// session's Off lifts under Cruise or under Land — is told `done` at its
/// start and nothing more over the same reply, and nothing is typed. The
/// window keeps a held face's time across the host's "unknown" when the
/// same face follows, and a move from open is what it stamps anew. NEGATIVE
/// CONTROLS: a typed row after the note seeds nothing (open, then done);
/// someone else's done turn that no loop watched (another reply's words)
/// withdraws the seed — told open, then the new done, which the window
/// stamps anew; a reply that moved on drops it (open).
#[test]
fn a_restarted_done_is_restated_and_typed_nothing() {
    // In place.
    let (dir, ledger) = finished_first("done-restart-plain");
    let host = Arc::new(Faces::default());
    let (lines, turns) = run(
        vec![ended(TOKEN_REPLY)],
        "cruise",
        Vec::new(),
        &host,
        Some(&ledger),
        None,
    );
    assert_eq!(
        (host.told(), turns),
        (vec![TaskFace::Done], 0),
        "{lines:#?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
    // As Off lifts, under Cruise and under Land, twice over.
    for word in ["cruise", "land"] {
        let (dir, ledger) = finished_first(&format!("done-restart-{word}"));
        for again in 0..2 {
            let (told, turns, added, lines) = after_off(&ledger, word, TOKEN_REPLY);
            let log = format!("{added:#?}\n{lines:#?}");
            assert_eq!(told, [TaskFace::Done], "{word} {again}: {log}");
            assert_eq!(turns, 0, "{word} {again}: nothing typed: {log}");
            assert!(!added.iter().any(|l| landed_note(l)), "{word}: {log}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
    // NEGATIVE CONTROL: a typed row after the note.
    let (dir, ledger) = finished_first("done-restart-typed");
    std::thread::sleep(Duration::from_millis(5));
    let mut rows = std::fs::read_to_string(&ledger).unwrap_or_default();
    rows.push_str(&ledger_row("continue@v1", "keep going", Duration::ZERO));
    rows.push('\n');
    std::fs::write(&ledger, rows).expect("ledger");
    let (told, _, _, lines) = after_off(&ledger, "cruise", TOKEN_REPLY);
    assert_eq!(told, [TaskFace::Open, TaskFace::Done], "{lines:#?}");
    let _ = std::fs::remove_dir_all(&dir);
    // NEGATIVE CONTROL: someone else's done turn no loop watched.
    let (dir, ledger) = finished_first("done-restart-theirs");
    let (told, turns, _, lines) = after_off(&ledger, "cruise", THEIR_TOKEN_REPLY);
    assert_eq!(
        (told, turns),
        (vec![TaskFace::Done, TaskFace::Open, TaskFace::Done], 0),
        "withdrawn, then the new done: {lines:#?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
    // NEGATIVE CONTROL: the reply moved on.
    let (dir, ledger) = finished_first("done-restart-moved");
    let (told, _, _, lines) = after_off(&ledger, "cruise", PROGRESS);
    assert_eq!(told, [TaskFace::Done, TaskFace::Open], "{lines:#?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The wall time (unix ms) of the last `DONE … rule=<rule>` note in the
/// journal beside `ledger`.
fn note_time(ledger: &Path, rule: &str) -> i64 {
    let (records, _) = crate::supervise::journal::read_journal(
        &crate::supervise::approvals::journal_beside(ledger),
    )
    .expect("journal");
    records
        .iter()
        .rev()
        .find(|r| r.line.starts_with("DONE ") && r.line.contains(&format!(" rule={rule} ")))
        .map(|r| r.t)
        .expect("the end's note")
}

/// A RESTATED END CARRIES THE TIME ITS NOTE WAS JOURNALED (§10.2.7; the
/// review of the done's seed: a seamless update's new window holds no face
/// of the session, so the done or the landing a restarted loop restated was
/// stamped with the update's clock there): a restarted loop over a Cruise
/// done, and over a landing whose note is nine minutes old, tells its start
/// face through `IdleHost::task_face_noted` with the `t` of the journal's
/// note of that end — and every other face through `task_face` alone.
/// NEGATIVE CONTROLS: a fresh loop, and a restart after a typed row (no
/// seed: open at the start), tell nothing with a time; a seed withdrawn at
/// the first point (another reply's done) tells only the restated start
/// with the note's time — the new done after it goes without, so a window
/// stamps it at its own time.
#[test]
fn a_restated_end_carries_the_time_its_note_was_journaled() {
    let min = |n: u64| Duration::from_secs(60 * n);
    // A done, restarted in place.
    let (dir, ledger) = finished_first("noted-done");
    let at = note_time(&ledger, "finished@v1");
    let host = Arc::new(Faces::default());
    let (lines, _) = run(
        vec![ended(TOKEN_REPLY)],
        "cruise",
        Vec::new(),
        &host,
        Some(&ledger),
        None,
    );
    assert_eq!(host.told(), [TaskFace::Done], "{lines:#?}");
    assert_eq!(host.noted(), [(TaskFace::Done, at)], "{lines:#?}");
    let _ = std::fs::remove_dir_all(&dir);
    // A landing noted nine minutes back, restarted under Land.
    let (dir, path) = files(
        "noted-landed",
        &[ledger_row(RULE_LAND, "land", min(10))],
        &[journal_row(LANDED_NOTE, min(9), None)],
    );
    let at = note_time(&path, RULE_LAND);
    assert!(at < crate::supervise::journal::unix_ms() - 8 * 60_000);
    let host = Arc::new(Faces::default());
    let (lines, turns) = run(
        vec![ended(TOKEN_REPLY)],
        "land",
        Vec::new(),
        &host,
        Some(&path),
        None,
    );
    assert_eq!(turns, 0, "{lines:#?}");
    assert_eq!(host.noted(), [(TaskFace::Landed, at)], "{lines:#?}");
    let _ = std::fs::remove_dir_all(&dir);
    // NEGATIVE CONTROL: a fresh loop.
    let (dir, path) = files("noted-fresh", &[], &[]);
    let host = Arc::new(Faces::default());
    run(
        vec![busy_screen(), ended(TOKEN_REPLY)],
        "cruise",
        Vec::new(),
        &host,
        Some(&path),
        None,
    );
    assert_eq!(host.told(), [TaskFace::Open, TaskFace::Done]);
    assert_eq!(host.noted(), []);
    let _ = std::fs::remove_dir_all(&dir);
    // NEGATIVE CONTROL: a typed row after the note.
    let (dir, ledger) = finished_first("noted-typed");
    std::thread::sleep(Duration::from_millis(5));
    let mut rows = std::fs::read_to_string(&ledger).unwrap_or_default();
    rows.push_str(&ledger_row("continue@v1", "keep going", Duration::ZERO));
    rows.push('\n');
    std::fs::write(&ledger, rows).expect("ledger");
    let host = Arc::new(Faces::default());
    run(
        vec![ended(TOKEN_REPLY)],
        "cruise",
        Vec::new(),
        &host,
        Some(&ledger),
        None,
    );
    assert_eq!(host.told()[0], TaskFace::Open);
    assert_eq!(host.noted(), []);
    let _ = std::fs::remove_dir_all(&dir);
    // NEGATIVE CONTROL: the seed withdrawn by another reply's done.
    let (dir, ledger) = finished_first("noted-theirs");
    let at = note_time(&ledger, "finished@v1");
    let host = Arc::new(Faces {
        lifted: true,
        ..Faces::default()
    });
    let (lines, _) = run(
        vec![ended(THEIR_TOKEN_REPLY)],
        "cruise",
        Vec::new(),
        &host,
        Some(&ledger),
        None,
    );
    assert_eq!(
        host.told(),
        [TaskFace::Done, TaskFace::Open, TaskFace::Done],
        "{lines:#?}"
    );
    assert_eq!(host.noted(), [(TaskFace::Done, at)], "only the start");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The last `DONE …` note in the journal beside `ledger`.
fn last_done(ledger: &Path) -> crate::supervise::journal::JournalRecord {
    let (records, _) = crate::supervise::journal::read_journal(
        &crate::supervise::approvals::journal_beside(ledger),
    )
    .expect("journal");
    records
        .into_iter()
        .rev()
        .find(|r| r.line.starts_with("DONE "))
        .expect("a DONE note")
}

/// A RESTATED END'S NOTE CARRIES ITS FIRST TIME FORWARD (§10.2.7): every
/// restarted loop journals the end again at its first point — its note must
/// be newer than the point record that loop wrote, or no loop after it could
/// seed — and that note keeps the time the end was FIRST noted
/// (`JournalRecord::ended`), so the loop after a restart and then another
/// (a seamless update, say) restates the done, and the landing, with the
/// first time rather than the last restart's. NEGATIVE CONTROLS: the first
/// loop's own note carries none; a done withdrawn by another reply's done is
/// a new end — its note carries none, and the loop after it restates it at
/// its own time.
#[test]
fn a_restated_ends_note_carries_its_first_time_forward() {
    let min = |n: u64| Duration::from_secs(60 * n);
    // A done, restarted twice.
    let (dir, ledger) = finished_first("chain-done");
    let first = last_done(&ledger);
    assert_eq!(first.ended, None, "the first loop's own end");
    for again in 0..2 {
        std::thread::sleep(Duration::from_millis(5));
        let host = Arc::new(Faces::default());
        let (lines, turns) = run(
            vec![ended(TOKEN_REPLY)],
            "cruise",
            Vec::new(),
            &host,
            Some(&ledger),
            None,
        );
        assert_eq!(turns, 0, "{again}: {lines:#?}");
        assert_eq!(
            host.noted(),
            [(TaskFace::Done, first.t)],
            "{again}: the first time: {lines:#?}"
        );
        let note = last_done(&ledger);
        assert!(note.t > first.t, "{again}: restated again: {note:?}");
        assert_eq!(note.ended, Some(first.t), "{again}: {note:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
    // A landing noted nine minutes back, restarted twice under Land.
    let (dir, path) = files(
        "chain-landed",
        &[ledger_row(RULE_LAND, "land", min(10))],
        &[journal_row(LANDED_NOTE, min(9), None)],
    );
    let at = note_time(&path, RULE_LAND);
    for again in 0..2 {
        let host = Arc::new(Faces::default());
        let (lines, turns) = run(
            vec![ended(TOKEN_REPLY)],
            "land",
            Vec::new(),
            &host,
            Some(&path),
            None,
        );
        assert_eq!(turns, 0, "{again}: {lines:#?}");
        assert_eq!(host.noted(), [(TaskFace::Landed, at)], "{again}");
        let note = last_done(&path);
        assert!(landed_note(&note.line), "{again}: {note:?}");
        assert_eq!(note.ended, Some(at), "{again}: {note:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
    // NEGATIVE CONTROL: another reply's done withdraws the seed.
    let (dir, ledger) = finished_first("chain-theirs");
    let host = Arc::new(Faces {
        lifted: true,
        ..Faces::default()
    });
    run(
        vec![ended(THEIR_TOKEN_REPLY)],
        "cruise",
        Vec::new(),
        &host,
        Some(&ledger),
        None,
    );
    assert_eq!(
        host.told(),
        [TaskFace::Done, TaskFace::Open, TaskFace::Done]
    );
    let theirs = last_done(&ledger);
    assert_eq!(theirs.ended, None, "a new end: {theirs:?}");
    let host = Arc::new(Faces::default());
    run(
        vec![ended(THEIR_TOKEN_REPLY)],
        "cruise",
        Vec::new(),
        &host,
        Some(&ledger),
        None,
    );
    assert_eq!(host.noted(), [(TaskFace::Done, theirs.t)], "its own time");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A RESTART OF A LANDED SESSION TYPES NOTHING (§9.4.5): the loop finds the
/// land's `DONE` as the journal's last end and no typed row after it, and the
/// screen's reply still saying done — it is told `landed` AT ITS START and
/// nothing after (the landing restated, never a move from open the window
/// would record as a new landing), nothing is typed, and it says the landing
/// once more in its journal. NEGATIVE CONTROLS: with a typed row after the
/// note, or no note (and no land act in the ledger), the start tells open and
/// the land instruction is typed into the finished work, as before the seed;
/// and a reply that no longer says done drops the seed — the landing told at
/// the start moves to open. THE LEDGER'S LAND EPISODE (RS7-2,
/// `approvals::land_episode`): the land instruction a loop before typed, no
/// note of its end, and its done reply on the screen — that loop never judged
/// it, so this one lands it there, as that loop would have: nothing typed,
/// open at the start and landed at the point (a move: no landing was said
/// before).
#[test]
fn a_restart_of_a_landed_session_types_no_land_instruction() {
    let min = |n: u64| Duration::from_secs(60 * n);
    let done = "All done. Nothing left to do.";
    let restart = |tag: &str, ledger: &[String], journal: &[String], said: &str| {
        let (dir, path) = files(tag, ledger, journal);
        let host = Arc::new(Faces::default());
        let (lines, turns) = run(
            vec![ended(said)],
            "land",
            Vec::new(),
            &host,
            Some(&path),
            None,
        );
        let notes: Vec<String> = crate::supervise::journal::read_journal(
            &crate::supervise::approvals::journal_beside(&path),
        )
        .expect("journal")
        .0
        .into_iter()
        .map(|r| r.line)
        .filter(|l| l.starts_with("DONE "))
        .collect();
        let _ = std::fs::remove_dir_all(&dir);
        (turns, host.told(), notes, lines)
    };
    let land_typed = ledger_row(RULE_LAND, "land", min(10));
    let note = journal_row(LANDED_NOTE, min(9), None);
    for said in [done, TOKEN_REPLY] {
        let (turns, told, notes, lines) = restart(
            "restart-landed",
            std::slice::from_ref(&land_typed),
            std::slice::from_ref(&note),
            said,
        );
        assert_eq!(turns, 0, "{said}: {lines:#?}");
        assert_eq!(told, [TaskFace::Landed], "{said}");
        assert_eq!(notes.len(), 2, "{said}: {notes:#?}");
        assert!(
            notes[1].contains(&format!("rule={RULE_LAND} ")),
            "{said}: {notes:#?}"
        );
    }
    // NEGATIVE CONTROLS: a typed row after the note; no note at all.
    let nudge = ledger_row("continue@v1", "keep going", min(2));
    for (tag, ledger, journal) in [
        (
            "restart-typed",
            vec![land_typed.clone(), nudge],
            vec![note.clone()],
        ),
        (
            "restart-no-note",
            vec![ledger_row("continue@v1", "keep going", min(10))],
            Vec::new(),
        ),
    ] {
        let (turns, told, _, lines) = restart(tag, &ledger, &journal, done);
        assert_eq!(turns, 1, "{tag}: the land instruction: {lines:#?}");
        assert!(
            lines
                .iter()
                .any(|l| l.contains(&format!("rule={RULE_LAND} "))),
            "{tag}: {lines:#?}"
        );
        assert_eq!(told, [TaskFace::Open], "{tag}");
    }
    // The land instruction typed, no note of its end: the ledger's land
    // episode holds it awaited, and its done reply lands the task here.
    let (turns, told, notes, lines) = restart(
        "restart-land-awaited",
        std::slice::from_ref(&land_typed),
        &[],
        done,
    );
    assert_eq!(turns, 0, "the land act's done reply: {lines:#?}");
    assert_eq!(told, [TaskFace::Open, TaskFace::Landed]);
    assert_eq!(notes.len(), 1, "{notes:#?}");
    assert!(
        notes[0].contains(&format!("rule={RULE_LAND} ")),
        "{notes:#?}"
    );
    // A reply that no longer says done: the seed goes, the point is decided
    // as any first point is.
    let (turns, told, _, lines) = restart("restart-moved-on", &[land_typed], &[note], PROGRESS);
    assert_eq!(turns, 1, "{lines:#?}");
    assert_eq!(told, [TaskFace::Landed, TaskFace::Open]);
}

/// The reply that ends SOMEONE ELSE'S TASK after a landing: their own job,
/// ended on the worker's token (the Together run's person's turn: a module
/// docstring, its tests and its commit).
const THEIR_TOKEN_REPLY: &str =
    "Added the module docstring, ran the tests and committed it.\nATERM-TASK-DONE";

/// The journal's lines beside `ledger`, in order.
fn journal_lines(ledger: &Path) -> Vec<String> {
    crate::supervise::journal::read_journal(&crate::supervise::approvals::journal_beside(ledger))
        .map(|(records, _)| records.into_iter().map(|r| r.line).collect())
        .unwrap_or_default()
}

/// Whether a line is the land's `DONE` note.
fn landed_note(line: &str) -> bool {
    line.starts_with("DONE ") && line.contains(&format!(" rule={RULE_LAND} "))
}

/// THE FIRST LOOP OF A LANDING: under Land, the job's progress, the land
/// instruction, its done reply — `DONE … rule=land@v1` — then `then` (a turn
/// of someone else's, say), over the ledger and journal at `ledger`, with a
/// person's hand on the script's screens `hand`. What the host was told and
/// the turns typed.
fn land_first(
    ledger: &Path,
    then: Vec<Vec<String>>,
    hand: &[usize],
) -> (Vec<TaskFace>, usize, Vec<String>) {
    let host = Arc::new(Faces::default());
    let mut screens = vec![
        busy_screen(),
        ended(PROGRESS),
        busy_screen(),
        ended(TOKEN_REPLY),
    ];
    screens.extend(then);
    let (lines, turns) = run_on(screens, "land", vec![1], &host, Some(ledger), None, |m| {
        for &i in hand {
            m.human_on.insert(i, "0");
        }
    });
    (host.told(), turns, lines)
}

/// A LOOP STARTED AS THE SESSION'S OFF LIFTED, under `word`, at the screen
/// `said`, over the same ledger and journal: what the host was told, the
/// turns typed, the lines it added to the journal, and what it said.
fn after_off(
    ledger: &Path,
    word: &'static str,
    said: &str,
) -> (Vec<TaskFace>, usize, Vec<String>, Vec<String>) {
    let (told, turns, added, lines, _) = after_off_on(ledger, word, vec![ended(said)], |_| {});
    (told, turns, added, lines)
}

/// [`after_off`] over `screens`, the mock set up further by `more` (the
/// timer's word, say) and handed back as the run left it.
fn after_off_on(
    ledger: &Path,
    word: &'static str,
    screens: Vec<Vec<String>>,
    more: impl FnOnce(&mut Mock),
) -> (Vec<TaskFace>, usize, Vec<String>, Vec<String>, Mock) {
    let before = journal_lines(ledger).len();
    let host = Arc::new(Faces {
        lifted: true,
        ..Faces::default()
    });
    let (lines, turns, m) = run_kept(screens, word, Vec::new(), &host, Some(ledger), None, more);
    let added = journal_lines(ledger).split_off(before);
    (host.told(), turns, added, lines, m)
}

/// [`THEIR_TOKEN_REPLY`] drawn again at another width: its words wrapped
/// onto other rows (a resize's redraw), a new point to the loop's key.
const THEIR_TOKEN_REPLY_REWRAPPED: &str =
    "Added the module docstring, ran the tests\nand committed it.\nATERM-TASK-DONE";

/// SOMEONE ELSE'S DONE TURN AFTER A LANDING IS NEVER READ AS THE LANDING
/// (Together D3, the real sequence of 2026-10-16 on Claude Code 2.1.302:
/// `DONE seq=41432 rule=land@v1`; a person's turn typed into Claude ended on
/// `ATERM-TASK-DONE`, `EVENT idle seq=46002`, `task=done`; Autopilot Off;
/// then Cruise with a timer, whose new loop journaled `DONE seq=46004
/// rule=land@v1 the worker said it is done after the land instruction` and
/// read `task=landed`; then the same again when Land was chosen). The first
/// loop lands the task and someone else's turn after it ends done, noting no
/// new end. The loop started as Off lifts under Cruise reads that turn in
/// the files beside the session — its point judged in the ledger
/// (`approvals::RULE_POINT`, after the landing's note) and in the journal
/// (its words not the landing's) — and seeds nothing: it starts open, its first point (the token) is done, noted
/// `DONE … rule=finished@v1`, and nothing is typed. THE TIMER FIRES there:
/// the word turns Land just after that first screen is read, and the reply
/// is drawn again at another width (a new point to the loop): the task is
/// already done (§9.3.7: nothing typed, `✓ done`), so no decision there
/// acts — the loop reads the word fresh only where one would — and no
/// landing is claimed. The loop after the next Off, under Land, reads the
/// same, restated from the Cruise loop's done note (the done's seed: told
/// `done` at its start, nothing more). No loop after the landing notes a
/// second `DONE … rule=land@v1`.
/// The same where that turn was a person's own exchange (their hand in it:
/// `theirs`). NEGATIVE CONTROLS:
/// `a_plain_landing_restarted_after_off_still_reads_landed` (a plain landing
/// restarted reads landed); the same turn where no loop watched it, in
/// neither file, is the next test, whose first point's words drop the seed.
#[test]
fn someone_elses_done_turn_after_a_landing_is_never_read_as_the_landing() {
    for (tag, hand, their_face) in [
        ("together-their-turn", &[][..], TaskFace::Done),
        ("together-own-exchange", &[4, 5][..], TaskFace::Theirs),
    ] {
        let (dir, ledger) = files(tag, &[], &[]);
        let (told, turns, lines) =
            land_first(&ledger, vec![busy_screen(), ended(THEIR_TOKEN_REPLY)], hand);
        let first = journal_lines(&ledger);
        let log = format!("{first:#?}\n{lines:#?}");
        assert_eq!(turns, 1, "{tag}: the land instruction alone: {log}");
        assert_eq!(
            told,
            [TaskFace::Open, TaskFace::Landed, TaskFace::Open, their_face],
            "{tag}: {log}"
        );
        let landed = first
            .iter()
            .position(|l| landed_note(l))
            .unwrap_or_else(|| panic!("{tag}: the landing: {log}"));
        assert!(
            first[landed + 1..]
                .iter()
                .any(|l| l.starts_with("EVENT idle ") && l.ends_with("ATERM-TASK-DONE")),
            "{tag}: their turn's point, after the landing: {log}"
        );
        // Off, then Cruise with a timer that fires; then Off again, and Land.
        for word in ["cruise", "land"] {
            let (told, turns, added, lines, m) = if word == "cruise" {
                after_off_on(
                    &ledger,
                    word,
                    vec![ended(THEIR_TOKEN_REPLY), ended(THEIR_TOKEN_REPLY_REWRAPPED)],
                    |m| {
                        m.autopilot_on.insert(0, Some("land"));
                    },
                )
            } else {
                after_off_on(&ledger, word, vec![ended(THEIR_TOKEN_REPLY)], |_| {})
            };
            let log = format!("{added:#?}\n{lines:#?}");
            assert_eq!(
                m.autopilot,
                Some("land"),
                "{tag} {word}: the word ends Land"
            );
            if word == "cruise" {
                assert_eq!(
                    added
                        .iter()
                        .filter(|l| l.starts_with("EVENT idle ") && l.ends_with("ATERM-TASK-DONE"))
                        .count(),
                    2,
                    "{tag}: the redraw after the timer is a point of its own: {log}"
                );
            }
            assert_eq!(turns, 0, "{tag} {word}: nothing typed: {log}");
            // Under Land the Cruise loop's done note seeds the done: told at
            // the start, its time kept, and kept by the same reply's words.
            let want: &[TaskFace] = if word == "cruise" {
                &[TaskFace::Open, TaskFace::Done]
            } else {
                &[TaskFace::Done]
            };
            assert_eq!(told, want, "{tag} {word}: done, never landed: {log}");
            assert_eq!(
                added
                    .iter()
                    .filter(|l| l.starts_with("DONE ") && l.contains(" rule=finished@v1 "))
                    .count(),
                1,
                "{tag} {word}: the token's end, once: {log}"
            );
            assert!(
                !added.iter().any(|l| landed_note(l)),
                "{tag} {word}: no land instruction preceded that reply: {log}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A DONE TURN NO LOOP WATCHED, AFTER A LANDING, READS DONE (the review of
/// Together D3, 2026-10-17): the first loop lands; then a turn is typed and
/// ends on the worker's token while Off has stopped the loop, so the journal
/// holds nothing of it. The loop started as Off lifts — under Cruise (a timer
/// armed), and under Land — arms the landing's seed from the journal, told
/// `landed` at its start as any seeded loop is; but its first point's last
/// words are not the landing's ([`said_mark`]): the seed is dropped there,
/// the token reads THEIR task's end — `done`, `DONE … rule=finished@v1` —
/// nothing is typed, and no `DONE … rule=land@v1` is noted over a reply no
/// land instruction preceded. NEGATIVE CONTROLS: the plain landing restarted
/// over its own reply reads landed (above); and this test with the words'
/// check taken out of `TurnEndState::observe` fails at `[Landed]` and the
/// false landed note.
#[test]
fn a_done_turn_no_loop_watched_after_a_landing_reads_done_not_landed() {
    for word in ["cruise", "land"] {
        let (dir, ledger) = files(&format!("unwatched-{word}"), &[], &[]);
        let (told, turns, lines) = land_first(&ledger, Vec::new(), &[]);
        assert_eq!(turns, 1, "{lines:#?}");
        assert_eq!(told, [TaskFace::Open, TaskFace::Landed], "{lines:#?}");
        let (told, turns, added, lines) = after_off(&ledger, word, THEIR_TOKEN_REPLY);
        let log = format!("{added:#?}\n{lines:#?}");
        assert_eq!(turns, 0, "{word}: nothing typed: {log}");
        assert_eq!(told, [TaskFace::Landed, TaskFace::Done], "{word}: {log}");
        assert!(
            added
                .iter()
                .any(|l| l.starts_with("DONE ") && l.contains(" rule=finished@v1 ")),
            "{word}: their task's end: {log}"
        );
        assert!(!added.iter().any(|l| landed_note(l)), "{word}: {log}");
        // …and the loop after it restates that end from its note: done at
        // its start (the done's seed, its time kept), nothing more told.
        let (told, turns, _, _) = after_off(&ledger, word, THEIR_TOKEN_REPLY);
        assert_eq!((told, turns), (vec![TaskFace::Done], 0), "{word}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A LANDING RIDDEN THROUGH A SEAMLESS UPDATE'S CUT STILL READS LANDED AFTER
/// ITS RESTART (the review of Together D3, 2026-10-17). After the landing an
/// update cuts the loop's wait (exit 75): the loop rides it out and hands the
/// same, unmoved point over again under the successor's seq, which starts
/// over (`RECONNECTED`, then `EVENT idle seq=9`). Read by its seq that point
/// was another one, so the next restart under Land dropped the seed: a
/// landing said in words alone got the land instruction typed into finished
/// work (the v1 defect §9.4.5's seed was made for), and one on the token read
/// `done`. Read by its words ([`said_mark`]) it is the landing's own: the
/// restart is told `landed`, types nothing and notes the landing once more,
/// for both. NEGATIVE CONTROL: this test with `landed_before` reading the
/// later points by `seq=` again fails at the restart's `[Open]` and its one
/// turn.
#[test]
fn a_landing_ridden_through_an_update_cut_reads_landed_after_its_restart() {
    const WORDS_DONE: &str = "Verified, committed and pushed the parser. All done.";
    for reply in [WORDS_DONE, TOKEN_REPLY] {
        // The cut's request index, after the landing: the first that leaves
        // the landing's point handed over again after the reconnect. Every
        // index is tried at once, each on a thread of its own: a run whose cut
        // misses waits its loop out.
        let cut = |i: usize| {
            let (dir, ledger) = files("update-cut", &[], &[]);
            let host = Arc::new(Faces::default());
            let screens = vec![busy_screen(), ended(PROGRESS), busy_screen(), ended(reply)];
            let (_, turns) = run_on(screens, "land", vec![1], &host, Some(&ledger), None, |m| {
                m.by_index.insert(i, replaced(REPLACED_ANCHOR));
                m.handoff_seq = Some(7);
            });
            let first = journal_lines(&ledger);
            let seq =
                |l: &str| crate::supervise::journal::JournalRecord::of_line(0, None, l, None).seq;
            let again = first.iter().position(|l| landed_note(l)).is_some_and(|k| {
                let after = &first[k + 1..];
                after
                    .iter()
                    .position(|l| l.starts_with("RECONNECTED"))
                    .is_some_and(|r| {
                        after[r + 1..].iter().any(|l| {
                            l.starts_with("EVENT idle ")
                                && seq(l).is_some()
                                && seq(l) != seq(&first[k])
                        })
                    })
            });
            if again && turns == 1 && host.told() == [TaskFace::Open, TaskFace::Landed] {
                Some((dir, ledger, first))
            } else {
                let _ = std::fs::remove_dir_all(&dir);
                None
            }
        };
        let cut = &cut;
        let mut handed: Vec<_> = std::thread::scope(|scope| {
            let tries: Vec<_> = (4..40usize).map(|i| scope.spawn(move || cut(i))).collect();
            tries
                .into_iter()
                .filter_map(|t| t.join().unwrap_or_else(|p| std::panic::resume_unwind(p)))
                .collect()
        });
        for (later, _, _) in handed.iter().skip(1) {
            let _ = std::fs::remove_dir_all(later);
        }
        handed.truncate(1);
        let (dir, ledger, first) = handed
            .pop()
            .unwrap_or_else(|| panic!("{reply:?}: no cut handed the landing's point again"));
        let (told, turns, added, lines) = after_off(&ledger, "land", reply);
        let log = format!("{first:#?}\n{added:#?}\n{lines:#?}");
        assert_eq!(
            turns, 0,
            "{reply:?}: nothing typed into finished work: {log}"
        );
        assert_eq!(told, [TaskFace::Landed], "{reply:?}: {log}");
        assert!(added.iter().any(|l| landed_note(l)), "{reply:?}: {log}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A PLAIN LANDING, RESTARTED, STILL READS LANDED (the negative control of
/// `someone_elses_done_turn_after_a_landing_is_never_read_as_the_landing` and
/// `a_done_turn_no_loop_watched_after_a_landing_reads_done_not_landed`, over
/// a journal and ledger the first loop wrote itself, not hand-made ones): with no turn after the landing, the loop started as
/// Off lifts — under Cruise (a timer armed), and under Land — seeds the
/// landing: told `landed` at its start and nothing after, nothing
/// typed, its journal saying the landing once more. What the first loop
/// journals after its landing, while that point stands, is no later point.
#[test]
fn a_plain_landing_restarted_after_off_still_reads_landed() {
    let (dir, ledger) = files("plain-landing", &[], &[]);
    let (told, turns, lines) = land_first(&ledger, Vec::new(), &[]);
    let first = journal_lines(&ledger);
    let log = format!("{first:#?}\n{lines:#?}");
    assert_eq!(turns, 1, "the land instruction alone: {log}");
    assert_eq!(told, [TaskFace::Open, TaskFace::Landed], "{log}");
    assert!(first.iter().any(|l| landed_note(l)), "{log}");
    for word in ["cruise", "land"] {
        let (told, turns, added, lines) = after_off(&ledger, word, TOKEN_REPLY);
        let log = format!("{added:#?}\n{lines:#?}");
        assert_eq!(turns, 0, "{word}: nothing typed: {log}");
        assert_eq!(told, [TaskFace::Landed], "{word}: {log}");
        assert!(added.iter().any(|l| landed_note(l)), "{word}: {log}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A PERSON'S TURN AFTER A LANDING THAT LEAVES THE TASK OPEN SEEDS OPEN:
/// their own exchange after the landing (their hand in it), whose reply
/// says no done — `theirs`, nothing typed after it. The loop after an Off
/// starts open, never `landed` for a moment, no landing is journaled, and
/// the exchange stays the person's (`theirs`, `LEFT … left-to-person@v1`):
/// nothing is typed until the next turn ends, as the loop that left it to
/// them would have. NEGATIVE CONTROL: the plain landing above reads landed.
#[test]
fn a_persons_turn_after_a_landing_that_leaves_the_task_open_seeds_open() {
    let (dir, ledger) = files("open-own-exchange", &[], &[]);
    let (told, turns, lines) = land_first(&ledger, vec![busy_screen(), ended(PROGRESS)], &[4, 5]);
    assert_eq!(turns, 1, "{lines:#?}");
    assert_eq!(
        told,
        [
            TaskFace::Open,
            TaskFace::Landed,
            TaskFace::Open,
            TaskFace::Theirs
        ],
        "{lines:#?}"
    );
    let (told, turns, added, lines) = after_off(&ledger, "land", PROGRESS);
    let log = format!("{added:#?}\n{lines:#?}");
    assert_eq!(
        told,
        [TaskFace::Open, TaskFace::Theirs],
        "open from the start, then the person's: {log}"
    );
    assert_eq!(turns, 0, "nothing typed into their exchange: {log}");
    assert!(
        added
            .iter()
            .any(|l| l.starts_with("LEFT ") && l.contains(" rule=left-to-person@v1 ")),
        "{log}"
    );
    assert!(!added.iter().any(|l| landed_note(l)), "{log}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A host owed one carry-on, as the window's host is after a relaunch
/// (`harness_host`'s `WorkerIdle`): it wants the first idle point, records
/// what the loop told it of that point ([`IdlePoint`]) and every face, and
/// answers as `relaunch::resume_as` does — nothing typed at a task done
/// under Land (`done:said-done`), else its continuation typed
/// (`continued`).
#[derive(Debug, Default)]
struct Carrier {
    points: Mutex<Vec<IdlePoint>>,
    faces: Mutex<Vec<TaskFace>>,
}

impl Carrier {
    fn points(&self) -> Vec<IdlePoint> {
        self.points
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
    fn faces(&self) -> Vec<TaskFace> {
        self.faces
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl IdleHost for Carrier {
    fn wants(&self) -> bool {
        self.points
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty()
    }
    fn at_idle(&self) -> Option<HostStep> {
        self.at_idle_on(IdlePoint::default())
    }
    fn at_idle_on(&self, point: IdlePoint) -> Option<HostStep> {
        self.points
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(point);
        let carried = !point.done_under_land;
        Some(HostStep {
            line: format!(
                "carry-on step={}",
                if carried {
                    "continued"
                } else {
                    crate::harness::relaunch::CARRIED_DONE
                }
            ),
            moved: carried,
            typed: carried.then_some(crate::supervise::policy::turn_end::HostTurn::CarryOn),
            repeat: false,
        })
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn task_face(&self, face: TaskFace) {
        self.faces
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(face);
    }
}

/// A relaunched agent's first point under the word `word`, its screen
/// `said` (the conversation it resumed), its carry-on owed to `host`, at
/// test speed, its approval ledger and journal at `ledger`: what the loop
/// said, the turns it typed, and its journal's lines. A cold restore's new
/// tab has both empty; a restart of the same session has its own.
fn relaunched(
    said: &str,
    word: &'static str,
    host: &Arc<Carrier>,
    ledger: &Path,
) -> (Vec<String>, usize, Vec<String>) {
    let mut m = Mock::new(vec![ended(said)]);
    m.autopilot = Some(word);
    m.vanish_after = Some(30);
    m.cursor_on_caret = true;
    m.stall_sleep = Some(Duration::from_millis(20));
    let journal = crate::supervise::approvals::journal_beside(ledger);
    let opts = SuperviseOpts {
        policy: SupervisorConfig {
            human_grace_s: 0,
            ..SupervisorConfig::default()
        },
        journal: Some(journal.clone()),
        idle_host: Some(Arc::clone(host) as Arc<dyn IdleHost>),
        ..auto(30)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            short_backoff: Duration::from_millis(30),
            short_backoff_max: Duration::from_millis(60),
            ..TurnEndTiming::default()
        });
        s.set_approval_ledger(Some(ledger.to_path_buf()));
    });
    let notes = crate::supervise::journal::read_journal(&journal)
        .map(|(records, _)| records.into_iter().map(|r| r.line).collect())
        .unwrap_or_default();
    (lines, count(&m, "turn"), notes)
}

/// A RELAUNCHED TASK DONE UNDER LAND GETS NO CARRY-ON (the Autopilot
/// design's §9.4.8): a cold restore's new tab — its journal and ledger
/// empty — whose resumed conversation's last reply is the worker's own
/// token is told done under Land at its first point; the host types nothing
/// (`HOST … carry-on step=done:said-done`), the policy ends the task after
/// it (`DONE … rule=finished@v1`), the face reads done, and the loop types
/// nothing either. A restart of the same session whose landing is seeded
/// is told so too: its reply in words alone reads landed, nothing typed.
/// NEGATIVE CONTROLS: a Land session mid-work, and a Cruise session whose
/// reply is the token, are told no such thing, and their carry-on is typed
/// as before.
#[test]
fn a_relaunched_task_done_under_land_is_told_so_and_typed_nothing() {
    let min = |n: u64| Duration::from_secs(60 * n);
    let carried = |tag: &str, said: &str, word: &'static str, seeded: bool| {
        let (dir, path) = if seeded {
            files(
                tag,
                &[ledger_row(RULE_LAND, "land", min(10))],
                &[journal_row(LANDED_NOTE, min(9), None)],
            )
        } else {
            files(tag, &[], &[])
        };
        let host = Arc::new(Carrier::default());
        let (lines, turns, notes) = relaunched(said, word, &host, &path);
        let _ = std::fs::remove_dir_all(&dir);
        (host.points(), host.faces(), turns, notes, lines)
    };
    let done = IdlePoint {
        done_under_land: true,
    };
    // A cold restore: the token on screen.
    let (points, faces, turns, notes, lines) = carried("carry-cold", TOKEN_REPLY, "land", false);
    assert_eq!(points, [done], "{notes:#?}");
    assert_eq!(turns, 0, "nothing typed: {lines:#?}");
    assert_eq!(faces, [TaskFace::Open, TaskFace::Done], "{notes:#?}");
    let host_line = notes
        .iter()
        .position(|l| l.starts_with("HOST ") && l.ends_with("carry-on step=done:said-done"))
        .unwrap_or_else(|| panic!("the host's word: {notes:#?}"));
    let end = notes
        .iter()
        .position(|l| l.starts_with("DONE ") && l.contains("rule=finished@v1"))
        .unwrap_or_else(|| panic!("the task's end: {notes:#?}"));
    assert!(host_line < end, "the host stepped first: {notes:#?}");
    // A restart of the same session, its landing seeded: told so, landed.
    let (points, faces, turns, notes, lines) = carried(
        "carry-landed",
        "All done. Nothing left to do.",
        "land",
        true,
    );
    assert_eq!(points, [done], "{notes:#?}");
    assert_eq!(turns, 0, "nothing typed: {lines:#?}");
    assert_eq!(faces, [TaskFace::Landed], "{notes:#?}");
    // NEGATIVE CONTROLS: mid-work under Land; the token under Cruise.
    for (tag, said, word) in [
        ("carry-mid-work", PROGRESS, "land"),
        ("carry-cruise", TOKEN_REPLY, "cruise"),
    ] {
        let (points, _, _, notes, _) = carried(tag, said, word, false);
        assert_eq!(points, [IdlePoint::default()], "{tag}: {notes:#?}");
        assert!(
            notes
                .iter()
                .any(|l| l.starts_with("HOST ") && l.ends_with("carry-on step=continued")),
            "{tag}: {notes:#?}"
        );
    }
}

/// A COLD RESTORE OF A LANDING WHOSE DONE WAS SAID IN WORDS ALONE: told done
/// under Land, so no carry-on is typed — but with no journal of its own to
/// seed the landing from, the point is a first done report, which Land
/// answers with its own instruction after the back-off (the remainder
/// §9.4.8 records). NEGATIVE CONTROL: the same reply after a restart whose
/// landing is seeded types nothing at all (the test above).
#[test]
fn a_cold_restore_of_a_landing_said_in_words_gets_the_land_instruction_not_the_carry_on() {
    let (dir, path) = files("carry-words", &[], &[]);
    let host = Arc::new(Carrier::default());
    let (lines, turns, notes) = relaunched("All done. Nothing left to do.", "land", &host, &path);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        host.points(),
        [IdlePoint {
            done_under_land: true
        }],
        "{notes:#?}"
    );
    assert_eq!(turns, 1, "the land instruction alone: {lines:#?}");
    assert!(
        notes
            .iter()
            .any(|l| l.contains(&format!("rule={RULE_LAND} "))),
        "{notes:#?}"
    );
    assert!(
        !notes.iter().any(|l| l.ends_with("carry-on step=continued")),
        "{notes:#?}"
    );
}

/// A host that restarts its agent for the memory banner, as the window's
/// host does (`harness_host`'s `IdleHost::restart`: `adopted`), and then owes
/// the relaunch's carry-on at the next idle point: it records what the loop
/// told it of that point ([`IdlePoint`]) and answers as `relaunch::resume_as`
/// does, as [`Carrier`] does.
#[derive(Debug, Default)]
struct Restarter {
    carry: std::sync::atomic::AtomicBool,
    restarts: Mutex<Vec<String>>,
    points: Mutex<Vec<IdlePoint>>,
}

impl IdleHost for Restarter {
    fn wants(&self) -> bool {
        self.carry.load(Ordering::SeqCst)
    }
    fn at_idle(&self) -> Option<HostStep> {
        self.at_idle_on(IdlePoint::default())
    }
    fn at_idle_on(&self, point: IdlePoint) -> Option<HostStep> {
        if !self.carry.swap(false, Ordering::SeqCst) {
            return None;
        }
        self.points
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(point);
        let step = if point.done_under_land {
            crate::harness::relaunch::CARRIED_DONE
        } else {
            "continued"
        };
        Some(HostStep {
            line: format!("carry-on step={step}"),
            moved: crate::harness::upgrade_drive::moved(step),
            typed: crate::harness::upgrade_drive::host_turn(step),
            repeat: false,
        })
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn can_restart(&self) -> bool {
        true
    }
    fn restart(&self, why: &crate::supervise::policy::turn_end::Restart) -> Option<String> {
        self.restarts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(why.word().to_string());
        self.carry.store(true, Ordering::SeqCst);
        Some("adopted".to_string())
    }
}

/// A LANDING THE MEMORY BANNER CUT IS CARRIED ON AFTER ITS RESTART (the
/// review of 2026-10-16, §9.4.8): under Land a job's done report gets the
/// land instruction, whose turn ends at Claude Code's memory banner; the
/// host restarts the agent in place, and at the relaunched agent's first
/// point, its resumed reply progress, the loop tells the host nothing of a
/// task done (the done report before the banner stands, but the banner's
/// reply said nothing), so the carry-on is typed (`continued`). NEGATIVE
/// CONTROL: the landing lands (its done reply), the banner comes after it,
/// and the relaunched agent's first point is told done under Land: nothing
/// typed (`done:said-done`).
#[test]
fn a_landing_the_memory_banner_cut_is_carried_on_after_its_restart() {
    use aterm_phase::prompt::fixtures::{MEMORY_BANNER_IDLE, screen};
    let done = "All done. Nothing left to do.";
    let restarted = |tag: &str, landing: Vec<Vec<String>>, after: &str| {
        let mut screens = vec![busy_screen(), ended(done), busy_screen()];
        screens.extend(landing);
        screens.extend(std::iter::repeat_n(screen(MEMORY_BANNER_IDLE), 3));
        screens.push(ended(after));
        let host = Arc::new(Restarter::default());
        let (dir, ledger) = files(tag, &[], &[]);
        let journal = crate::supervise::approvals::journal_beside(&ledger);
        let mut m = Mock::new(screens);
        m.autopilot = Some("land");
        m.turn_gates = vec![1];
        m.vanish_after = Some(30);
        m.cursor_on_caret = true;
        m.stall_sleep = Some(Duration::from_millis(20));
        let opts = SuperviseOpts {
            policy: SupervisorConfig {
                human_grace_s: 0,
                ..SupervisorConfig::default()
            },
            journal: Some(journal.clone()),
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            ..auto(30)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_turn_end_timing(TurnEndTiming {
                short_backoff: Duration::from_millis(30),
                short_backoff_max: Duration::from_millis(60),
                restart_backoff: vec![Duration::from_millis(30)],
                ..TurnEndTiming::default()
            });
            s.set_approval_ledger(Some(ledger.clone()));
        });
        let notes: Vec<String> = crate::supervise::journal::read_journal(&journal)
            .map(|(records, _)| records.into_iter().map(|r| r.line).collect())
            .unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        let points = host
            .points
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let restarts = host
            .restarts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        (points, restarts, notes, lines)
    };
    let host_said = |notes: &[String], step: &str| {
        notes
            .iter()
            .any(|l| l.starts_with("HOST ") && l.ends_with(&format!("carry-on step={step}")))
    };
    // The landing cut by the banner, its agent restarted mid-landing.
    let (points, restarts, notes, lines) = restarted("carry-cut", Vec::new(), PROGRESS);
    let log = format!("{notes:#?}\n{lines:#?}");
    assert!(
        notes
            .iter()
            .any(|l| l.contains(&format!("rule={RULE_LAND} "))),
        "the land instruction typed: {log}"
    );
    assert!(
        restarts.iter().any(|w| w == "memory"),
        "{restarts:?}: {log}"
    );
    assert_eq!(points.first(), Some(&IdlePoint::default()), "{log}");
    assert!(host_said(&notes, "continued"), "{log}");
    assert!(
        !notes.iter().any(|l| l.contains("rule=land-nudge@v1")),
        "no land nudge in the carry-on's place: {log}"
    );
    // NEGATIVE CONTROL: landed before the banner; nothing typed after it.
    let (points, restarts, notes, lines) =
        restarted("carry-landed-then-banner", vec![ended(done)], done);
    let log = format!("{notes:#?}\n{lines:#?}");
    assert!(
        restarts.iter().any(|w| w == "memory"),
        "{restarts:?}: {log}"
    );
    assert_eq!(
        points.first(),
        Some(&IdlePoint {
            done_under_land: true
        }),
        "{log}"
    );
    assert!(host_said(&notes, "done:said-done"), "{log}");
    let made = notes
        .iter()
        .position(|l| l.starts_with("RESTARTED "))
        .unwrap_or_else(|| panic!("the restart: {log}"));
    assert!(
        !notes[made..].iter().any(|l| l.starts_with("CONTINUED ")),
        "nothing typed after the restart: {log}"
    );
}
