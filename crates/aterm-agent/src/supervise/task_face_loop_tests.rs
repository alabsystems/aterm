// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// THE TASK'S FACE in the loop, over the scripted server
// (`docs/DESIGN-harness-autopilot-2026-10-15.md` §9.4.2, §9.4.5): what a
// recording host is told through `IdleHost::task_face` — once at the loop's
// start, then where a point moves it (landed, theirs) and at the
// first live read of someone else's turn — and the landing a loop before
// this one landed, read back from the journal beside the ledger
// (`ended_before`, with the time its note was journaled; an older build's
// done note seeds nothing), so a restart types no
// land instruction into finished work, and never takes someone else's later
// done turn for the landing. Each
// behaviour has its negative control. Included from `run_tests.rs` (`mod
// task_face_loop`), so the `Mock` and its helpers are in scope.

use crate::harness::acts::TaskFace;
use crate::supervise::policy::turn_end::{RULE_DONE_CHECK, RULE_LAND, RULE_LAND_NUDGE, RULE_LEFT};

/// The landing's clean defense, as Land's instruction asks for it.
const LANDING_REPLY: &str = "Committed and pushed the parser and its tests.\nNothing left: all of \
                             it is merged into the remote main branch.";
/// A reply that says no done.
const PROGRESS: &str = "Committing the parser now; the lexer is next.";

/// A host that records every face it is told, and how it started the loop
/// ([`IdleHost::lifted_off`]).
#[derive(Debug, Default)]
struct Faces {
    told: Mutex<Vec<TaskFace>>,
    /// What the conversation's record says of the landing
    /// ([`IdleHost::finished`]).
    finished: Option<bool>,
    /// What the repository probe finds ([`IdleHost::repo_work`]), and how
    /// often it was asked.
    repo: Option<crate::harness::repo_work::RepoWork>,
    probes: std::sync::atomic::AtomicUsize,
    /// What the probe finds at each ask in turn, before [`Self::repo`]
    /// answers every later one.
    repos: Mutex<std::collections::VecDeque<Option<crate::harness::repo_work::RepoWork>>>,
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
    fn finished(&self) -> Option<bool> {
        self.finished
    }
    fn repo_work(&self) -> Option<crate::harness::repo_work::RepoWork> {
        self.probes
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.repos
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front()
            .unwrap_or_else(|| self.repo.clone())
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

/// [`ended`] under the `❯` row of the prompt `prompt` it answers: the turn
/// that prompt opened, closed on the screen.
fn answered(prompt: &str, said: &str) -> Vec<String> {
    let mut r = rows(&[&format!("❯ {prompt}"), ""]);
    r.extend(ended(said));
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
    m.history_served();
    more(&mut m);
    let opts = SuperviseOpts {
        policy: SupervisorConfig {
            human_grace_s: 0,
            ..owners_policy()
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

/// LAND, ITS CLEAN DEFENSE: told `open` once at the loop's start, `landed`
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
            ended(LANDING_REPLY),
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

/// A LANDED TASK'S OWN WAKE KEEPS ITS FACE: after the landing, a turn the
/// conversation's record says nobody asked (Claude Code's wake for a
/// background task), read running and then ended on a quiet report, tells no
/// face — `landed` stands through its live frames and its point, and nothing
/// is typed. The recheck of d88a7bf289 found its first live read told `open`
/// and its point `landed` again, which the window logged as a fresh landing
/// at every wake. NEGATIVE CONTROL: with no record to say so, the same turn
/// is someone else's — told `open` at its first live read.
#[test]
fn a_landed_tasks_own_wake_tells_no_face() {
    let wake = |finished: Option<bool>| {
        let host = Arc::new(Faces {
            finished,
            ..Faces::default()
        });
        let screens = vec![
            busy_screen(),
            ended(PROGRESS),
            busy_screen(),
            ended(LANDING_REPLY),
            busy_screen(),
            ended("The monitor fired: the deploy finished."),
        ];
        let (lines, turns) = run(screens, "land", vec![1], &host, None, None);
        (host.told(), turns, lines)
    };
    let (told, turns, lines) = wake(Some(true));
    assert_eq!(told, [TaskFace::Open, TaskFace::Landed], "{lines:#?}");
    assert_eq!(turns, 1, "the instruction alone: {lines:#?}");
    // NEGATIVE CONTROL: no record.
    let (told, _, lines) = wake(None);
    assert_eq!(
        told.get(..3),
        Some(&[TaskFace::Open, TaskFace::Landed, TaskFace::Open][..]),
        "{lines:#?}"
    );
}

/// A LANDED TASK'S OWN WAKE OVER SOMEONE ELSE'S EDITS KEEPS THE LANDING
/// (`Session::took` probes as the wake begins,
/// `TurnEndState::wake_began_over`): the landing over a clean repository,
/// then the person's hand edit, then Claude Code's background wake the
/// record says nobody asked, ended on a quiet report over the same edit —
/// nothing typed, the face landed throughout. The recheck of ea564676a3
/// found the landing reopened and the worker told to commit the person's
/// edit. NEGATIVE CONTROL: the edit made during the wake (the probe clean
/// as it began) reopens the landing: Land's nudge.
#[test]
fn a_landed_tasks_own_wake_over_someone_elses_edit_keeps_the_landing() {
    let edit = crate::harness::repo_work::RepoWork {
        repo: "/Users//a/Developer/parser".to_string(),
        changed: 1,
        paths: vec!["README.md".to_string()],
        state: Some(0x5eed),
        ..Default::default()
    };
    let wake = |began: Option<crate::harness::repo_work::RepoWork>| {
        let host = Arc::new(Faces {
            finished: Some(true),
            repo: Some(edit.clone()),
            repos: Mutex::new([None, None, began].into_iter().collect()),
            ..Faces::default()
        });
        let screens = vec![
            busy_screen(),
            ended(PROGRESS),
            busy_screen(),
            ended(LANDING_REPLY),
            busy_screen(),
            ended("The monitor fired: the deploy finished."),
        ];
        let (lines, turns) = run(screens, "land", vec![1], &host, None, None);
        (host.told(), turns, lines)
    };
    let (told, turns, lines) = wake(Some(edit.clone()));
    assert_eq!(turns, 1, "the instruction alone: {lines:#?}");
    assert_eq!(told, [TaskFace::Open, TaskFace::Landed], "{lines:#?}");
    // NEGATIVE CONTROL: the edit made during the wake.
    let (told, turns, lines) = wake(None);
    assert_eq!(turns, 2, "the nudge: {lines:#?}");
    assert!(
        lines
            .iter()
            .any(|l| l.contains(&format!("rule={RULE_LAND_NUDGE} "))),
        "{lines:#?}"
    );
    assert_eq!(
        told,
        [TaskFace::Open, TaskFace::Landed, TaskFace::Open],
        "{lines:#?}"
    );
}

/// A RESTART WHOSE FIRST READ IS THE LANDED TASK'S OWN WAKE KEEPS THE
/// LANDING (`TurnEndState::wake_may_keep_landing` for the journal's seed):
/// the loop restarted over a landing (the land act typed, the landing's
/// `DONE` noted after it), its first read the background wake the record
/// says nobody asked, running and then ended on a quiet report: told
/// `landed` at its start and nothing more, nothing typed. The recheck of
/// ea564676a3 found the seed dropped at the wake's first frame and Land's
/// act typed into the landed work. NEGATIVE CONTROL: with no record to say
/// so, the wake is someone's turn — told `open`, and Land's act typed.
#[test]
fn a_restart_whose_first_read_is_the_landed_tasks_own_wake_keeps_the_landing() {
    let min = |n: u64| Duration::from_secs(60 * n);
    let restart = |tag: &str, finished: Option<bool>| {
        let (dir, path) = files(
            tag,
            &[ledger_row(RULE_LAND, "land", min(10))],
            &[journal_row(LANDED_NOTE, min(9), None)],
        );
        let host = Arc::new(Faces {
            finished,
            ..Faces::default()
        });
        let screens = vec![
            busy_screen(),
            ended("The monitor fired: the deploy finished."),
        ];
        let (lines, turns) = run(screens, "land", Vec::new(), &host, Some(&path), None);
        let _ = std::fs::remove_dir_all(&dir);
        (host.told(), turns, lines)
    };
    let (told, turns, lines) = restart("restart-wake", Some(true));
    assert_eq!(turns, 0, "nothing typed: {lines:#?}");
    assert_eq!(told, [TaskFace::Landed], "{lines:#?}");
    // NEGATIVE CONTROL: no record.
    let (told, turns, lines) = restart("restart-wake-unknown", None);
    assert_eq!(turns, 1, "Land's act: {lines:#?}");
    assert_eq!(told, [TaskFace::Landed, TaskFace::Open], "{lines:#?}");
}

/// CRUISE TELLS NO END: the retired token after a continuation, and the done
/// check's second done report, are each a done claim owed the check again —
/// told nothing past the start's `open`. A person's quick greeting under
/// Land is told `theirs` (the journal's `LEFT`), never `landed`.
#[test]
fn a_cruise_done_claim_tells_no_end_and_a_persons_exchange_is_theirs() {
    for (last, check) in [
        ("Committed and pushed.\nATERM-TASK-DONE", false),
        ("All done. Nothing left to do.", true),
    ] {
        let host = Arc::new(Faces::default());
        let mut screens = vec![busy_screen(), ended(PROGRESS), busy_screen(), ended(last)];
        let mut gates = vec![1];
        if check {
            // The done report gets the check, and the check's reply says
            // done again: the check again.
            screens.extend([busy_screen(), ended(last)]);
            gates.push(3);
        }
        let (lines, turns) = run(screens, "cruise", gates, &host, None, None);
        assert_eq!(turns, 2 + usize::from(check), "{last}: {lines:#?}");
        assert!(
            lines
                .iter()
                .any(|l| l.contains(&format!("rule={RULE_DONE_CHECK} "))),
            "{lines:#?}"
        );
        assert!(!lines.iter().any(|l| l.starts_with("DONE ")), "{lines:#?}");
        assert_eq!(host.told(), [TaskFace::Open], "{last}");
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

/// FOREVER LAND IN THE LOOP: the instruction and three nudges answered with
/// work, and a fourth nudge typed — told nothing past `open` (no spent face:
/// the landing goes on). NEGATIVE CONTROL: the third nudge's clean defense
/// is told `landed`, nothing more is typed, and someone else's live turn
/// after it reads open.
#[test]
fn a_landing_goes_on_past_two_nudges_and_is_told_landed_only_on_its_defense() {
    let land_on = |last: &str| {
        let host = Arc::new(Faces::default());
        let screens = vec![
            busy_screen(),
            ended(PROGRESS),
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
        let (lines, turns) = run(screens, "land", vec![1, 3, 5, 7], &host, None, None);
        (turns, host.told(), lines)
    };
    let (turns, told, lines) = land_on(PROGRESS);
    assert_eq!(turns, 5, "the instruction and four nudges: {lines:#?}");
    assert_eq!(told, [TaskFace::Open], "{lines:#?}");
    // NEGATIVE CONTROL: the defense lands it.
    let (turns, told, lines) = land_on(LANDING_REPLY);
    assert_eq!(turns, 4, "the instruction and three nudges: {lines:#?}");
    assert_eq!(
        told,
        [TaskFace::Open, TaskFace::Landed, TaskFace::Open],
        "{lines:#?}"
    );
}

/// A RESTART OF A LANDED TASK TELLS `landed` AT ITS START and types nothing
/// (the journal's land `DONE`, read back by `landed_before`), however many
/// nudges the landing took. NEGATIVE CONTROL: a restart of a landing still
/// going (its last reply progress) is told `open` at its start.
#[test]
fn a_restart_of_a_landing_tells_landed_and_types_nothing() {
    for (last, face) in [
        (LANDING_REPLY, TaskFace::Landed),
        (PROGRESS, TaskFace::Open),
    ] {
        let (dir, path) = files("restart-landed", &[], &[]);
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
        assert!(turns >= 3, "the instruction and two nudges: {lines:#?}");
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
        if face == TaskFace::Landed {
            assert_eq!(turns, 0, "{last}: {lines:#?}");
        }
        assert_eq!(host.told().first(), Some(&face), "{last}: {lines:#?}");
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

/// ONLY A LANDING IS READ BACK AS AN END (`ended_before`, §10.2.7 under
/// FOREVER CRUISE / FOREVER LAND): the last note of a task's end is the
/// land's `DONE`, nothing typed or judged came after it — the landing, its
/// point's mark with it, and the last of several notes is the one read.
/// NEGATIVE CONTROLS: an older build's done notes (`rule=finished@v1`, the
/// done check's, the open-items challenge's) read as nothing ended, alone and
/// as the last note after a landing; a person's `LEFT` as the last end, a
/// typed row after the note, another reply's point after it, and another
/// session's note each seed nothing.
#[test]
fn only_a_landing_is_read_back_and_an_older_builds_done_seeds_nothing() {
    let min = |n: u64| Duration::from_secs(60 * n);
    let mark = said_mark(Some(LANDING_REPLY));
    let theirs = said_mark(Some(PROGRESS));
    assert!(mark.is_some() && mark != theirs);
    let finished = "DONE seq=104 rule=finished@v1 the worker reported ATERM-TASK-DONE: its task \
         is finished; nothing more is typed until a person or an orchestrator types";
    let check = "DONE seq=104 rule=continue-done-check@v1 the worker said it is done after the \
         done check: nothing more is typed until a person or an orchestrator types";
    let open_items = "DONE seq=104 rule=continue-open-items@v1 the worker said it is done after \
         the open-items challenge: nothing more is typed until a person or an orchestrator types";
    let left = "LEFT seq=104 rule=left-to-person@v1 a person's own exchange (their message, \
         answered within 10 min): nothing is typed until the next turn ends";
    let point = "EVENT idle seq=104 Nothing left: all of it is merged into the remote main branch.";
    let read =
        |tag: &str, ledger: &[String], notes: &[&str], after: &[String], sid: Option<&str>| {
            // Each note after its point, as the loop writes them.
            let mut journal: Vec<String> = notes
                .iter()
                .flat_map(|n| {
                    [
                        marked_row(point, min(9), sid, mark),
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
            let got =
                ended_before(&journal, None, typed.last().copied()).map(|(words, _, _)| words);
            let _ = std::fs::remove_dir_all(&dir);
            got
        };
    let earlier = [ledger_row(RULE_LAND, "land", min(10))];
    assert_eq!(
        read("ended-land", &earlier, &[LANDED_NOTE], &[], None),
        Some(mark)
    );
    assert_eq!(
        read(
            "ended-done-then-land",
            &earlier,
            &[finished, LANDED_NOTE],
            &[],
            None
        ),
        Some(mark),
        "the last end is the one read"
    );
    let own = marked_row(point, min(5), None, mark);
    assert_eq!(
        read("ended-own-point", &earlier, &[LANDED_NOTE], &[own], None),
        Some(mark),
        "its own point said again keeps it"
    );
    // NEGATIVE CONTROLS: an older build's done seeds nothing.
    for note in [finished, check, open_items] {
        assert_eq!(
            read("ended-old-done", &earlier, &[note], &[], None),
            None,
            "{note}"
        );
    }
    assert_eq!(
        read(
            "ended-land-then-done",
            &earlier,
            &[LANDED_NOTE, finished],
            &[],
            None
        ),
        None,
        "an older build's done after the landing is the last end: nothing ended"
    );
    assert_eq!(
        read("ended-left", &earlier, &[LANDED_NOTE, left], &[], None),
        None
    );
    let typed = [
        earlier[0].clone(),
        ledger_row("continue@v1", "keep going", min(2)),
    ];
    assert_eq!(read("ended-typed", &typed, &[LANDED_NOTE], &[], None), None);
    let their_point = marked_row("EVENT idle seq=9 progress", min(5), None, theirs);
    assert_eq!(
        read(
            "ended-their-point",
            &earlier,
            &[LANDED_NOTE],
            &[their_point],
            None
        ),
        None
    );
    assert_eq!(
        read("ended-sid", &earlier, &[LANDED_NOTE], &[], Some("s-other")),
        None
    );
}

/// AN OLDER BUILD'S CRUISE DONE IS NEVER RESTATED (FOREVER CRUISE: nothing
/// ends a Cruise task, and an old journal's `DONE … rule=finished@v1` reads
/// as nothing ended): a loop started over that note, under Cruise or as the
/// session's Off lifts under Land, is told `open` at its start, never
/// `landed`, and tells no note's time. NEGATIVE CONTROL: the same journal
/// with the land's `DONE` as its last note restates the landing, its time
/// with it.
#[test]
fn an_older_builds_done_note_is_never_restated() {
    let min = |n: u64| Duration::from_secs(60 * n);
    let finished = "DONE seq=104 rule=finished@v1 the worker reported ATERM-TASK-DONE: its task \
         is finished; nothing more is typed until a person or an orchestrator types";
    for (word, lifted) in [("cruise", false), ("land", true)] {
        let (dir, path) = files(
            &format!("old-done-{word}"),
            &[ledger_row("continue@v1", "keep going", min(10))],
            &[journal_row(finished, min(9), None)],
        );
        let host = Arc::new(Faces {
            lifted,
            ..Faces::default()
        });
        let (lines, _) = run(
            vec![ended(LANDING_REPLY)],
            word,
            Vec::new(),
            &host,
            Some(&path),
            None,
        );
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            host.told().first(),
            Some(&TaskFace::Open),
            "{word}: {lines:#?}"
        );
        assert_eq!(host.noted(), [], "{word}: {lines:#?}");
        if word == "cruise" {
            assert!(!host.told().contains(&TaskFace::Landed), "{lines:#?}");
        }
    }
    // NEGATIVE CONTROL: the land's note is restated.
    let (dir, path) = files(
        "old-done-control",
        &[ledger_row(RULE_LAND, "land", min(10))],
        &[journal_row(LANDED_NOTE, min(9), None)],
    );
    let host = Arc::new(Faces::default());
    let (lines, turns) = run(
        vec![ended(LANDING_REPLY)],
        "land",
        Vec::new(),
        &host,
        Some(&path),
        None,
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(turns, 0, "{lines:#?}");
    assert_eq!(host.told(), [TaskFace::Landed], "{lines:#?}");
    assert_eq!(host.noted().len(), 1, "{lines:#?}");
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

/// A RESTATED LANDING CARRIES THE TIME ITS NOTE WAS JOURNALED (§10.2.7: a
/// seamless update's new window holds no face of the session, so the landing
/// a restarted loop restated was stamped with the update's clock there): a
/// restarted loop over a real landing, and over a landing whose note is nine
/// minutes old, tells its start face through `IdleHost::task_face_noted` with
/// the `t` of the journal's note — and every other face through `task_face`
/// alone. NEGATIVE CONTROLS: a fresh loop, and a restart after a typed row
/// (no seed: open at the start), tell nothing with a time.
#[test]
fn a_restated_landing_carries_the_time_its_note_was_journaled() {
    let min = |n: u64| Duration::from_secs(60 * n);
    // A real landing, restarted in place.
    let (dir, ledger) = files("noted-real", &[], &[]);
    let (told, turns, lines) = land_first(&ledger, Vec::new(), &[]);
    assert_eq!(
        (told, turns),
        (vec![TaskFace::Open, TaskFace::Landed], 1),
        "{lines:#?}"
    );
    let at = note_time(&ledger, RULE_LAND);
    let host = Arc::new(Faces::default());
    let (lines, turns) = run(
        vec![ended(LANDING_REPLY)],
        "land",
        Vec::new(),
        &host,
        Some(&ledger),
        None,
    );
    assert_eq!(turns, 0, "{lines:#?}");
    assert_eq!(host.told(), [TaskFace::Landed], "{lines:#?}");
    assert_eq!(host.noted(), [(TaskFace::Landed, at)], "{lines:#?}");
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
        vec![ended(LANDING_REPLY)],
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
        vec![busy_screen(), ended(PROGRESS)],
        "cruise",
        Vec::new(),
        &host,
        Some(&path),
        None,
    );
    assert_eq!(host.told().first(), Some(&TaskFace::Open));
    assert_eq!(host.noted(), []);
    let _ = std::fs::remove_dir_all(&dir);
    // NEGATIVE CONTROL: a typed row after the note.
    let (dir, ledger) = files("noted-typed", &[], &[]);
    land_first(&ledger, Vec::new(), &[]);
    std::thread::sleep(Duration::from_millis(5));
    let mut rows = std::fs::read_to_string(&ledger).unwrap_or_default();
    rows.push_str(&ledger_row("continue@v1", "keep going", Duration::ZERO));
    rows.push('\n');
    std::fs::write(&ledger, rows).expect("ledger");
    let host = Arc::new(Faces::default());
    run(
        vec![ended(LANDING_REPLY)],
        "land",
        Vec::new(),
        &host,
        Some(&ledger),
        None,
    );
    assert_eq!(host.told()[0], TaskFace::Open);
    assert_eq!(host.noted(), []);
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

/// A RESTATED LANDING'S NOTE CARRIES ITS FIRST TIME FORWARD (§10.2.7): every
/// restarted loop journals the landing again at its first point — its note
/// must be newer than the point record that loop wrote, or no loop after it
/// could seed — and that note keeps the time the landing was FIRST noted
/// (`JournalRecord::ended`), so the loop after a restart and then another (a
/// seamless update, say) restates the landing with the first time rather
/// than the last restart's. NEGATIVE CONTROL: the first loop's own note
/// carries none.
#[test]
fn a_restated_landings_note_carries_its_first_time_forward() {
    let min = |n: u64| Duration::from_secs(60 * n);
    // A real landing, restarted twice.
    let (dir, ledger) = files("chain-real", &[], &[]);
    land_first(&ledger, Vec::new(), &[]);
    let first = last_done(&ledger);
    assert!(landed_note(&first.line), "{first:?}");
    assert_eq!(first.ended, None, "the first loop's own end");
    for again in 0..2 {
        std::thread::sleep(Duration::from_millis(5));
        let host = Arc::new(Faces::default());
        let (lines, turns) = run(
            vec![ended(LANDING_REPLY)],
            "land",
            Vec::new(),
            &host,
            Some(&ledger),
            None,
        );
        assert_eq!(turns, 0, "{again}: {lines:#?}");
        assert_eq!(
            host.noted(),
            [(TaskFace::Landed, first.t)],
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
            vec![ended(LANDING_REPLY)],
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
}

/// THE PAGE COUNTS EACH LANDING ONCE, FROM THE FILES THE LOOPS WROTE (design
/// §12.4.3, item 2; slice 4b): a real landing kept through the task's own
/// wake (its note restated, `WAKE_KEPT`), then two loops restarted over it
/// (a seamless update, a relaunch), each restating it with its first time
/// (`ended`) — the page's one reading of the ledger and the journal
/// (`stats_windows`) counts ONE landing beside the one land prompt, today, in
/// the week and in the session's share, after every loop: across the
/// restarts the count is the journal's. Someone else's turn then reopens the
/// task and Land's next act lands it again: two. NEGATIVE CONTROLS: the
/// journal holds four `DONE … rule=land@v1` notes of the first landing (a
/// count of the notes reads 4), and a window opened after the first landing
/// counts only the second.
#[test]
fn the_page_counts_each_landing_once_through_its_wake_and_restarts() {
    use crate::harness::stats::stats_windows;
    use crate::supervise::policy::turn_end::WAKE_KEPT;
    const WAKE: &str = "The monitor fired: the deploy finished.";
    let dir = crate::supervise::test_scratch_path("task-face", "landed-count");
    let _ = std::fs::remove_dir_all(&dir);
    let state = dir.join("state");
    let drive = state.join("drive");
    std::fs::create_dir_all(&drive).expect("drive");
    let ledger = drive.join("s-land.jsonl");
    std::fs::write(&ledger, "").expect("ledger");
    std::fs::write(crate::supervise::approvals::journal_beside(&ledger), "").expect("journal");
    let opened = crate::supervise::journal::unix_ms() - 3_600_000;
    // (today's landings, the week's, the session's share, today's land prompts)
    let read = |since: i64| {
        let (today, week) = stats_windows(&state, &dir.join("harness"), since, since - 86_400_000);
        let share: u64 = today
            .by_session
            .as_ref()
            .map_or(0, |s| s.values().map(|t| t.landed).sum());
        (today.landed, week.landed, share, today.land_prompts)
    };
    let notes = || {
        journal_lines(&ledger)
            .iter()
            .filter(|l| landed_note(l))
            .count()
    };
    let wakeful = || {
        Arc::new(Faces {
            finished: Some(true),
            ..Faces::default()
        })
    };
    // The first loop: the land instruction, its clean defense, the wake.
    let host = wakeful();
    let screens = vec![
        busy_screen(),
        ended(PROGRESS),
        busy_screen(),
        ended(LANDING_REPLY),
        busy_screen(),
        ended(WAKE),
    ];
    let (lines, turns) = run(screens, "land", vec![1], &host, Some(&ledger), None);
    assert_eq!(turns, 1, "the instruction alone: {lines:#?}");
    assert_eq!(
        host.told(),
        [TaskFace::Open, TaskFace::Landed],
        "{lines:#?}"
    );
    let first = journal_lines(&ledger);
    assert!(
        first
            .iter()
            .any(|l| landed_note(l) && l.contains(WAKE_KEPT)),
        "the wake restated the landing: {first:#?}"
    );
    assert_eq!(read(opened), (1, 1, 1, 1), "{first:#?}");
    // Two loops restarted over it, each restating it.
    for again in 0..2 {
        std::thread::sleep(Duration::from_millis(5));
        let host = wakeful();
        let (lines, turns) = run(
            vec![ended(WAKE)],
            "land",
            Vec::new(),
            &host,
            Some(&ledger),
            None,
        );
        assert_eq!(turns, 0, "{again}: {lines:#?}");
        assert_eq!(host.told(), [TaskFace::Landed], "{again}: {lines:#?}");
        assert_eq!(read(opened), (1, 1, 1, 1), "{again}: the journal's count");
    }
    // NEGATIVE CONTROL: four notes of the one landing.
    assert_eq!(notes(), 4, "{:#?}", journal_lines(&ledger));
    // Someone else's turn reopens the task, and Land's act lands it again.
    std::thread::sleep(Duration::from_millis(5));
    let reopened = crate::supervise::journal::unix_ms();
    let host = Arc::new(Faces::default());
    let screens = vec![
        busy_screen(),
        ended(PROGRESS),
        busy_screen(),
        ended(LANDING_REPLY),
    ];
    let (lines, turns) = run(screens, "land", vec![1], &host, Some(&ledger), None);
    assert_eq!(turns, 1, "Land's act: {lines:#?}");
    assert_eq!(host.told().last(), Some(&TaskFace::Landed), "{lines:#?}");
    let (today, week, share, prompts) = read(opened);
    assert_eq!(
        (today, week, share),
        (2, 2, 2),
        "{:#?}",
        journal_lines(&ledger)
    );
    assert_eq!(prompts, 2, "two land prompts, one per landing");
    // NEGATIVE CONTROL: a window opened after the first landing.
    assert_eq!(read(reopened).0, 1, "{:#?}", journal_lines(&ledger));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A RESTART OF A LANDED SESSION TYPES NOTHING (§9.4.5): the loop finds the
/// land's `DONE` as the journal's last end and no typed row after it, and the
/// screen's reply still a clean defense — it is told `landed` AT ITS START and
/// nothing after (the landing restated, never a move from open the window
/// would record as a new landing), nothing is typed, and it says the landing
/// once more in its journal. NEGATIVE CONTROLS: with a typed row after the
/// note, or no note (and no land act in the ledger), the start tells open and
/// the land instruction is typed into the finished work, as before the seed;
/// and a reply that no longer says done drops the seed — the landing told at
/// the start moves to open — as does a done that is no clean defense (an
/// older build's note, which has no words to compare, at a done in words:
/// the landing's nudge owed, the review of 2026-10-20). THE LEDGER'S LAND
/// EPISODE (RS7-2,
/// `approvals::land_episode`): the land instruction a loop before typed, no
/// note of its end, and its clean defense on the screen — that loop never judged
/// it, so this one lands it there, as that loop would have: nothing typed,
/// open at the start and landed at the point (a move: no landing was said
/// before).
#[test]
fn a_restart_of_a_landed_session_types_no_land_instruction() {
    let min = |n: u64| Duration::from_secs(60 * n);
    let done = "All done. Nothing left to do.";
    let restart_on = |tag: &str, ledger: &[String], journal: &[String], screen: Vec<String>| {
        let (dir, path) = files(tag, ledger, journal);
        let host = Arc::new(Faces::default());
        let (lines, turns) = run(vec![screen], "land", Vec::new(), &host, Some(&path), None);
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
    let restart = |tag: &str, ledger: &[String], journal: &[String], said: &str| {
        restart_on(tag, ledger, journal, ended(said))
    };
    let land_typed = ledger_row(RULE_LAND, "land", min(10));
    let note = journal_row(LANDED_NOTE, min(9), None);
    let defended = "All of it is in.\nNothing left: merged into the remote main branch.";
    for said in [defended, LANDING_REPLY] {
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
        let (turns, told, _, lines) = restart(tag, &ledger, &journal, defended);
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
    // episode holds it awaited, and its clean defense — the act's own `❯`
    // row over it, so the act was taken — lands the task here.
    let (turns, told, notes, lines) = restart_on(
        "restart-land-awaited",
        std::slice::from_ref(&land_typed),
        &[],
        answered("land", LANDING_REPLY),
    );
    assert_eq!(turns, 0, "the land act's defense: {lines:#?}");
    assert_eq!(told, [TaskFace::Open, TaskFace::Landed]);
    assert_eq!(notes.len(), 1, "{notes:#?}");
    assert!(
        notes[0].contains(&format!("rule={RULE_LAND} ")),
        "{notes:#?}"
    );
    // NEGATIVE CONTROL: the same defense with no `❯` row of the act over it
    // (said before the act, which a quit kept from being taken): no landing,
    // and Land's act is typed again.
    let (turns, told, notes, lines) = restart(
        "restart-land-untaken",
        std::slice::from_ref(&land_typed),
        &[],
        LANDING_REPLY,
    );
    assert_eq!(turns, 1, "Land's act again: {lines:#?}");
    assert!(
        lines
            .iter()
            .any(|l| l.contains(&format!("rule={RULE_LAND_NUDGE} "))),
        "{lines:#?}"
    );
    assert_eq!(told, [TaskFace::Open]);
    assert!(notes.is_empty(), "{notes:#?}");
    // A reply that no longer says done: the seed goes, the point is decided
    // as any first point is.
    let (turns, told, _, lines) = restart(
        "restart-moved-on",
        std::slice::from_ref(&land_typed),
        std::slice::from_ref(&note),
        PROGRESS,
    );
    assert_eq!(turns, 1, "{lines:#?}");
    assert_eq!(told, [TaskFace::Landed, TaskFace::Open]);
    // A done that is no defense: the seed goes, the land nudge is typed.
    let (turns, told, _, lines) = restart("restart-undefended", &[land_typed], &[note], done);
    assert_eq!(turns, 1, "{lines:#?}");
    assert!(
        lines
            .iter()
            .any(|l| l.contains(&format!("rule={RULE_LAND_NUDGE} "))),
        "{lines:#?}"
    );
    assert_eq!(told, [TaskFace::Landed, TaskFace::Open]);
}

/// A RESTART OVER SOMEONE ELSE'S EDITS KEEPS THE LANDING AND ASKS NO
/// REPOSITORY (`Session::repo_read`, `TurnEndState::landing_words_read`):
/// the journal's landing, the restarted loop's first point the landing's own
/// words (the mark its point kept) with no turn run since, over a checkout
/// that now holds work: the probe is never asked, nothing is typed, the face
/// is landed. The recheck of d88a7bf289 found the restarted loop probed,
/// reopened the landed worker and typed `land@v1`. NEGATIVE CONTROLS: a turn
/// run after it re-probes, and over that work Land's nudge is typed; and a
/// loop started as the session's Off lifted — a person's act, which re-arms
/// Land (`TurnEndState::park_off`) — asks the repository, and over the same
/// work the landing is over: Land's instruction again, the face open.
#[test]
fn a_restart_over_someone_elses_edits_keeps_the_landing_and_asks_no_repository() {
    let min = |n: u64| Duration::from_secs(60 * n);
    let mark = said_mark(Some(LANDING_REPLY));
    let dirty = crate::harness::repo_work::RepoWork {
        repo: "/Users//a/Developer/parser".to_string(),
        changed: 2,
        paths: vec!["src/lexer.rs".to_string()],
        ..Default::default()
    };
    let restart_as = |tag: &str, screens: Vec<Vec<String>>, gates: Vec<usize>, lifted: bool| {
        let (dir, path) = files(
            tag,
            &[ledger_row(RULE_LAND, "land", min(10))],
            &[
                marked_row("EVENT idle seq=104 landed", min(9), None, mark),
                journal_row(LANDED_NOTE, min(9), None),
            ],
        );
        let host = Arc::new(Faces {
            repo: Some(dirty.clone()),
            lifted,
            ..Faces::default()
        });
        let (lines, turns) = run(screens, "land", gates, &host, Some(&path), None);
        let _ = std::fs::remove_dir_all(&dir);
        let probes = host.probes.load(std::sync::atomic::Ordering::SeqCst);
        (turns, host.told(), probes, lines)
    };
    let restart = |tag: &str, screens: Vec<Vec<String>>, gates: Vec<usize>| {
        restart_as(tag, screens, gates, false)
    };
    let (turns, told, probes, lines) =
        restart("restart-edited", vec![ended(LANDING_REPLY)], Vec::new());
    assert_eq!(turns, 0, "{lines:#?}");
    assert_eq!(told, [TaskFace::Landed], "{lines:#?}");
    assert_eq!(probes, 0, "no repository asked: {lines:#?}");
    // NEGATIVE CONTROL: a turn run since.
    let (turns, told, probes, lines) = restart(
        "restart-edited-turn",
        vec![ended(LANDING_REPLY), busy_screen(), ended(LANDING_REPLY)],
        Vec::new(),
    );
    assert!(probes > 0, "{lines:#?}");
    assert_eq!(turns, 1, "{lines:#?}");
    assert!(
        lines
            .iter()
            .any(|l| l.contains(&format!("rule={RULE_LAND_NUDGE} "))),
        "{lines:#?}"
    );
    assert_eq!(told.first(), Some(&TaskFace::Landed), "{lines:#?}");
    assert!(told.contains(&TaskFace::Open), "{lines:#?}");
    // NEGATIVE CONTROL: the loop started as the session's Off lifted.
    let (turns, told, probes, lines) = restart_as(
        "restart-edited-off",
        vec![ended(LANDING_REPLY)],
        Vec::new(),
        true,
    );
    assert!(probes > 0, "the repository asked after an Off: {lines:#?}");
    assert_eq!(turns, 1, "{lines:#?}");
    assert!(
        lines
            .iter()
            .any(|l| l.contains(&format!("rule={RULE_LAND} "))),
        "Land's instruction again: {lines:#?}"
    );
    assert_eq!(told.last(), Some(&TaskFace::Open), "{told:?}: {lines:#?}");
}

/// The reply that ends SOMEONE ELSE'S TASK after a landing: their own job,
/// ended on the worker's token (the Together run's person's turn: a module
/// docstring, its tests and its commit).
const THEIR_LANDING_REPLY: &str =
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
        ended(LANDING_REPLY),
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

/// [`THEIR_LANDING_REPLY`] drawn again at another width: its words wrapped
/// onto other rows (a resize's redraw), a new point to the loop's key.
const THEIR_LANDING_REPLY_REWRAPPED: &str =
    "Added the module docstring, ran the tests\nand committed it.\nATERM-TASK-DONE";

/// SOMEONE ELSE'S DONE TURN AFTER A LANDING IS NEVER READ AS THE LANDING
/// (Together D3, the real sequence of 2026-10-16 on Claude Code 2.1.302:
/// `DONE seq=41432 rule=land@v1`; a person's turn typed into Claude ended on
/// `ATERM-TASK-DONE`, `EVENT idle seq=46002`; Autopilot Off; then Cruise
/// with a timer, whose new loop journaled `DONE seq=46004 rule=land@v1 the
/// worker said it is done after the land instruction` and read
/// `task=landed`; then the same again when Land was chosen). The first loop
/// lands the task and someone else's turn after it ends on a done claim:
/// open again (or theirs, their hand in it), never landed. The loops started
/// as Off lifts — under Cruise with a timer that turns the word Land, and
/// under Land — read that turn in the files beside the session (its point
/// judged in the ledger, its words not the landing's) and are never told
/// `landed`, and no loop after the landing notes a second `DONE …
/// rule=land@v1`. NEGATIVE CONTROLS:
/// `a_plain_landing_restarted_after_off_still_reads_landed` (a plain landing
/// restarted reads landed); the same turn where no loop watched it, in
/// neither file, is the next test, whose first point's words drop the seed.
#[test]
fn someone_elses_done_turn_after_a_landing_is_never_read_as_the_landing() {
    for (tag, hand) in [
        ("together-their-turn", &[][..]),
        ("together-own-exchange", &[4, 5][..]),
    ] {
        let (dir, ledger) = files(tag, &[], &[]);
        let (told, _, lines) = land_first(
            &ledger,
            vec![busy_screen(), ended(THEIR_LANDING_REPLY)],
            hand,
        );
        let first = journal_lines(&ledger);
        let log = format!("{first:#?}\n{lines:#?}");
        assert!(
            told.starts_with(&[TaskFace::Open, TaskFace::Landed, TaskFace::Open]),
            "{tag}: {log}"
        );
        assert!(
            !told[2..].contains(&TaskFace::Landed),
            "{tag}: their turn is no landing: {log}"
        );
        if !hand.is_empty() {
            assert_eq!(told.last(), Some(&TaskFace::Theirs), "{tag}: {log}");
        }
        assert_eq!(
            first.iter().filter(|l| landed_note(l)).count(),
            1,
            "{tag}: the landing, once: {log}"
        );
        // Off, then Cruise with a timer that fires; then Off again, and Land.
        for word in ["cruise", "land"] {
            let (told, _, added, lines, m) = if word == "cruise" {
                after_off_on(
                    &ledger,
                    word,
                    vec![
                        ended(THEIR_LANDING_REPLY),
                        ended(THEIR_LANDING_REPLY_REWRAPPED),
                    ],
                    |m| {
                        m.autopilot_on.insert(0, Some("land"));
                    },
                )
            } else {
                after_off_on(&ledger, word, vec![ended(THEIR_LANDING_REPLY)], |_| {})
            };
            let log = format!("{added:#?}\n{lines:#?}");
            assert_eq!(
                m.autopilot,
                Some("land"),
                "{tag} {word}: the word ends Land"
            );
            assert!(
                !told.contains(&TaskFace::Landed),
                "{tag} {word}: never landed: {log}"
            );
            assert!(
                !added.iter().any(|l| landed_note(l)),
                "{tag} {word}: no land act preceded that reply: {log}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A DONE TURN NO LOOP WATCHED, AFTER A LANDING, IS NO LANDING (the review of
/// Together D3, 2026-10-17): the first loop lands; then a turn is typed and
/// ends on the worker's token while Off has stopped the loop, so the journal
/// holds nothing of it. The loop started as Off lifts under Land arms the
/// landing's seed from the journal, told `landed` at its start as any seeded
/// loop under Land is; but its first point's last words are not the
/// landing's ([`said_mark`]): the seed is dropped there, the face is open,
/// and no `DONE … rule=land@v1` is noted over a reply no land act preceded.
/// Under Cruise (a timer armed) the seed is dropped before the start face
/// ([`TurnEndState::start_under`]): `open` only. NEGATIVE CONTROLS: the
/// plain landing restarted over its own reply reads landed (below); and this
/// test with the words' check taken out of `TurnEndState::observe` fails at
/// `[Landed]` and the false landed note.
#[test]
fn a_done_turn_no_loop_watched_after_a_landing_is_no_landing() {
    for word in ["cruise", "land"] {
        let (dir, ledger) = files(&format!("unwatched-{word}"), &[], &[]);
        let (told, turns, lines) = land_first(&ledger, Vec::new(), &[]);
        assert_eq!(turns, 1, "{lines:#?}");
        assert_eq!(told, [TaskFace::Open, TaskFace::Landed], "{lines:#?}");
        let (told, _, added, lines) = after_off(&ledger, word, THEIR_LANDING_REPLY);
        let log = format!("{added:#?}\n{lines:#?}");
        let start = if word == "land" {
            vec![TaskFace::Landed, TaskFace::Open]
        } else {
            vec![TaskFace::Open]
        };
        assert_eq!(told, start, "{word}: {log}");
        assert!(!added.iter().any(|l| landed_note(l)), "{word}: {log}");
        // …and the loop after it seeds nothing.
        let (told, _, _, _) = after_off(&ledger, word, THEIR_LANDING_REPLY);
        assert_eq!(told.first(), Some(&TaskFace::Open), "{word}");
        assert!(!told.contains(&TaskFace::Landed), "{word}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A LANDING RIDDEN THROUGH A SEAMLESS UPDATE'S CUT STILL READS LANDED AFTER
/// ITS RESTART (the review of Together D3, 2026-10-17). After the landing an
/// update cuts the loop's wait (exit 75): the loop rides it out and hands the
/// same, unmoved point over again under the successor's seq, which starts
/// over (`RECONNECTED`, then `EVENT idle seq=9`). Read by its seq that point
/// was another one, so the next restart under Land dropped the seed: the
/// landing's defense got the land instruction typed into landed work (the v1
/// defect §9.4.5's seed was made for). Read by its words ([`said_mark`]) it
/// is the landing's own: the restart is told `landed`, types nothing and
/// notes the landing once more, for each defense. NEGATIVE CONTROL: this
/// test with `landed_before` reading the later points by `seq=` again fails
/// at the restart's `[Open]` and its one turn.
#[test]
fn a_landing_ridden_through_an_update_cut_reads_landed_after_its_restart() {
    const SHORT_DEFENSE: &str =
        "Verified, committed and pushed the parser.\nNothing left: it is all on remote main.";
    for reply in [SHORT_DEFENSE, LANDING_REPLY] {
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
        assert_eq!(turns, 0, "{reply:?}: nothing typed into landed work: {log}");
        assert_eq!(told, [TaskFace::Landed], "{reply:?}: {log}");
        assert!(added.iter().any(|l| landed_note(l)), "{reply:?}: {log}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A PLAIN LANDING, RESTARTED, STILL READS LANDED (the negative control of
/// `someone_elses_done_turn_after_a_landing_is_never_read_as_the_landing` and
/// `a_done_turn_no_loop_watched_after_a_landing_reads_done_not_landed`, over
/// a journal and ledger the first loop wrote itself, not hand-made ones):
/// with no turn after the landing, the loop started as Off lifts under Land
/// seeds the landing: told `landed` at its start and nothing after, nothing
/// typed, its journal saying the landing once more. What the first loop
/// journals after its landing, while that point stands, is no later point.
/// Under Cruise (a timer armed) the landed session goes on: Cruise takes
/// nothing as the end, so the landing's defense is a done claim, owed the
/// done check, and the face the loop leaves told is `open`.
#[test]
fn a_plain_landing_restarted_after_off_still_reads_landed() {
    let (dir, ledger) = files("plain-landing", &[], &[]);
    let (told, turns, lines) = land_first(&ledger, Vec::new(), &[]);
    let first = journal_lines(&ledger);
    let log = format!("{first:#?}\n{lines:#?}");
    assert_eq!(turns, 1, "the land instruction alone: {log}");
    assert_eq!(told, [TaskFace::Open, TaskFace::Landed], "{log}");
    assert!(first.iter().any(|l| landed_note(l)), "{log}");
    let (told, turns, added, lines) = after_off(&ledger, "land", LANDING_REPLY);
    let log = format!("{added:#?}\n{lines:#?}");
    assert_eq!(turns, 0, "land: nothing typed: {log}");
    assert_eq!(told, [TaskFace::Landed], "land: {log}");
    assert!(added.iter().any(|l| landed_note(l)), "land: {log}");
    let (told, turns, added, lines) = after_off(&ledger, "cruise", LANDING_REPLY);
    let log = format!("{added:#?}\n{lines:#?}");
    assert!(turns >= 1, "cruise: the done check: {log}");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("CONTINUED ") && l.contains(" rule=continue-done-check@v1 ")),
        "cruise: {log}"
    );
    assert_eq!(
        told.last(),
        Some(&TaskFace::Open),
        "cruise: {told:?}: {log}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE HOST THAT TAKES THE TASK'S END AND STOPS ITS LOOP THERE: it wants the
/// idle point after the `at`-th turn ran — the point the task ends at — and
/// takes it with a step that moves the session (the live upgrade's quiet
/// restart, `done:resumed`), raising the loop's stop as it steps, as the
/// window's host stops a worker whose session reads Off, whose SUPERVISION
/// setting was saved, or whose aterm updates itself. Every face is recorded.
#[derive(Debug)]
struct EndTaker {
    at: usize,
    ran: std::sync::atomic::AtomicUsize,
    taken: AtomicBool,
    stop: Arc<AtomicBool>,
    faces: Mutex<Vec<TaskFace>>,
}

impl EndTaker {
    fn new(at: usize, stop: &Arc<AtomicBool>) -> Self {
        Self {
            at,
            ran: std::sync::atomic::AtomicUsize::new(0),
            taken: AtomicBool::new(false),
            stop: Arc::clone(stop),
            faces: Mutex::new(Vec::new()),
        }
    }
}

impl IdleHost for EndTaker {
    fn wants(&self) -> bool {
        !self.taken.load(Ordering::SeqCst) && self.ran.load(Ordering::SeqCst) >= self.at
    }
    fn at_idle(&self) -> Option<HostStep> {
        self.taken.store(true, Ordering::SeqCst);
        self.stop.store(true, Ordering::SeqCst);
        Some(HostStep {
            line: "upgrade step=done:resumed".to_string(),
            moved: true,
            typed: None,
            repeat: false,
        })
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn turn_ran(&self) {
        self.ran.fetch_add(1, Ordering::SeqCst);
    }
    fn task_face(&self, face: TaskFace) {
        self.faces
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(face);
    }
}

/// HOW THE FIRST LOOP OF A TASK THAT ENDS IS CUT at the task's end
/// ([`ended_first_cut`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cut {
    /// Not cut: no host step comes, and the loop runs on to the session's
    /// end.
    No,
    /// The host's step takes the end's point and stops the loop there
    /// ([`EndTaker`]).
    HostStep,
    /// The reader of the loop's lines goes away as the end's point line is
    /// printed: that print and every one after it fail ([`ReaderGone`]), as
    /// a CLI `aterm supervise watch` piped into a reader that quit.
    ReaderGone,
    /// The host hangs the loop's connection up as the fold publishes the
    /// end (`task mark done …`, [`Mock::hang_up_at`]): that request and
    /// every one after it fail in the transport, as the window's host cuts
    /// a loop short to stop it at once.
    HungUp,
}

/// THE LOOP'S STDOUT, whose reader goes away at its `ok + 1`-th `EVENT
/// idle` line: that write and every one after it fail (`BrokenPipe`).
#[derive(Debug)]
struct ReaderGone {
    ok: usize,
    gone: bool,
    out: Vec<u8>,
}

impl std::io::Write for ReaderGone {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if !self.gone && buf.starts_with(b"EVENT idle ") {
            self.gone = self.ok == 0;
            self.ok = self.ok.saturating_sub(1);
        }
        if self.gone {
            return Err(std::io::ErrorKind::BrokenPipe.into());
        }
        self.out.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        if self.gone {
            return Err(std::io::ErrorKind::BrokenPipe.into());
        }
        Ok(())
    }
}

/// THE FIRST LOOP OF A TASK THAT ENDS, under Land, over the ledger and
/// journal at `ledger`: the job's progress, Land's instruction the policy
/// types there, then `last`, its answer (FOREVER LAND: the one end is a
/// landing; Cruise never ends). With `take`, the host takes the end's point
/// and stops the loop there ([`EndTaker`]); else no host step comes. The
/// faces told, the turns typed, the lines said.
fn ended_first(ledger: &Path, last: &str, take: bool) -> (Vec<TaskFace>, usize, Vec<String>) {
    ended_first_cut(ledger, last, if take { Cut::HostStep } else { Cut::No })
}

/// [`ended_first`], the loop cut at the task's end as `cut` says.
fn ended_first_cut(ledger: &Path, last: &str, cut: Cut) -> (Vec<TaskFace>, usize, Vec<String>) {
    let word = "land";
    let screens = vec![busy_screen(), ended(PROGRESS), busy_screen(), ended(last)];
    let gates = vec![1];
    let take = cut == Cut::HostStep;
    let stop = Arc::new(AtomicBool::new(false));
    let host = Arc::new(EndTaker::new(if take { 2 } else { usize::MAX }, &stop));
    let mut m = Mock::new(screens);
    m.history_served();
    m.autopilot = Some(word);
    m.turn_gates = gates;
    m.vanish_after = Some(6);
    m.cursor_on_caret = true;
    m.stall_sleep = Some(Duration::from_millis(20));
    // The published end: a manager's worker, its record `working`.
    let tasks = (cut == Cut::HungUp).then(|| ledger.with_file_name("tasks"));
    if tasks.is_some() {
        m.task = Some((
            1,
            "state=working from=s-0a1b since_ms=0 result_bytes=0".to_string(),
        ));
        m.hang_up_at = Some("task mark");
    }
    let opts = SuperviseOpts {
        policy: SupervisorConfig {
            human_grace_s: 0,
            ..SupervisorConfig::default()
        },
        journal: Some(crate::supervise::approvals::journal_beside(ledger)),
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        tasks,
        ..auto(30)
    };
    let setup = |s: &mut Session<'_, TapedCtl<'_, Mock>>| {
        s.stop = Some(stop);
        s.set_turn_end_timing(TurnEndTiming {
            short_backoff: Duration::from_millis(30),
            short_backoff_max: Duration::from_millis(60),
            ..TurnEndTiming::default()
        });
        s.set_approval_ledger(Some(ledger.to_path_buf()));
    };
    let lines = if cut == Cut::ReaderGone {
        // The points before the end's print; the end's own fails.
        let mut out = ReaderGone {
            ok: 1,
            gone: false,
            out: Vec::new(),
        };
        let (tape, mut taped) = memory_tape(&mut m);
        let mut s = taped_session(&mut taped, &tape);
        s.set_clock(TEST_NOW, PDT);
        s.set_zone_lookup(test_zones);
        setup(&mut s);
        let _ = s.watch(&opts, &mut out);
        drop(s);
        drop(taped);
        assert!(
            out.gone,
            "{word} {last:?}: the reader went away at the end's point"
        );
        String::from_utf8(out.out)
            .expect("utf-8")
            .lines()
            .map(str::to_string)
            .collect()
    } else {
        watch_lines_with(&mut m, &opts, setup).0
    };
    assert_eq!(
        host.taken.load(Ordering::SeqCst),
        take,
        "{word} {last:?}: the host took the end: {lines:#?}"
    );
    assert_eq!(
        m.hung_up,
        cut == Cut::HungUp,
        "{word} {last:?}: the host hung up as the end was published: {lines:#?}"
    );
    let faces = host
        .faces
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    (faces, count(&m, "turn"), lines)
}

/// The follower of a landing's books, on them alone ([`run`], its screen
/// the landing's reply): under Land — restarted in place, or started as the
/// session's Off lifts — it restates the landing at its start with the first
/// note's time and types nothing; under Cruise (started as Off lifts) the
/// landed session goes on, its defense a done claim owed the done check, the
/// face it leaves told `open`.
fn follow_landing(ledger: &Path, tag: &str, follower: &'static str, lifted: bool) {
    let at = note_time(ledger, RULE_LAND);
    let host = Arc::new(Faces {
        lifted,
        ..Faces::default()
    });
    let before = journal_lines(ledger).len();
    let (lines, turns) = run(
        vec![ended(LANDING_REPLY)],
        follower,
        Vec::new(),
        &host,
        Some(ledger),
        None,
    );
    let added = journal_lines(ledger).split_off(before);
    let log = format!("{tag}: {added:#?}\n{lines:#?}");
    if follower == "land" {
        assert_eq!(turns, 0, "nothing typed into the landed work: {log}");
        assert_eq!(
            host.told(),
            [TaskFace::Landed],
            "the landing restated: {log}"
        );
        assert_eq!(
            host.noted(),
            [(TaskFace::Landed, at)],
            "its first time: {log}"
        );
    } else {
        assert!(turns >= 1, "Cruise goes on: the done check: {log}");
        assert_eq!(
            host.told().last(),
            Some(&TaskFace::Open),
            "Cruise ends nothing: {log}"
        );
    }
}

/// A LANDING THE HOST'S STEP TOOK IS NOTED, AND A LOOP THAT FOLLOWS READS IT
/// AS THE FIRST LOOP DID (the claim of 2026-10-20 against main's `look()`:
/// the end's point journaled, its `DONE …` note owed to the point's act,
/// which the host's step there skips). Under FOREVER LAND the one end is a
/// landing: the host's step takes the very point the task lands at — the
/// clean defense answering Land's instruction — and the loop is stopped as
/// it steps. The journal still says the landing right after its point's
/// line, before the host's step; and a loop that follows on those books —
/// restarted in place, or started as the session's Off lifts under Land —
/// restates the landing at its start with the first note's time and types
/// nothing ([`follow_landing`]). Before the fix no note was written: every
/// follower started `open` and stamped the end with its own clock, and a
/// landing whose Land episode ended with the Off its loop was stopped under
/// got the land instruction again, typed into the finished work. A follower
/// started under Cruise goes on, as Cruise does over any landing. NEGATIVE
/// CONTROL: the same first loop with no host step notes the landing, and its
/// followers read the same.
#[test]
fn a_tasks_end_the_hosts_step_took_is_noted_and_its_follower_types_nothing() {
    for take in [true, false] {
        // Each follower on the first loop's books alone: a follower's own
        // note would seed the next.
        for (follower, lifted) in [("land", false), ("land", true), ("cruise", true)] {
            let tag = format!("end-taken-{take}-{follower}-{lifted}");
            let (dir, ledger) = files(&tag, &[], &[]);
            let (told, turns, lines) = ended_first(&ledger, LANDING_REPLY, take);
            let first = journal_lines(&ledger);
            let log = format!("{tag}: {first:#?}\n{lines:#?}");
            assert_eq!(turns, 1, "the land instruction alone: {log}");
            assert_eq!(told, [TaskFace::Open, TaskFace::Landed], "{log}");
            let end = first
                .iter()
                .position(|l| l.starts_with("DONE ") && l.contains(&format!(" rule={RULE_LAND} ")))
                .unwrap_or_else(|| panic!("the landing's note: {log}"));
            assert!(
                end > 0 && first[end - 1].starts_with("EVENT idle "),
                "the landing's point, then its note: {log}"
            );
            if take {
                assert!(
                    first[end + 1..]
                        .iter()
                        .any(|l| l.starts_with("HOST ") && l.ends_with(" step=done:resumed")),
                    "the host's step after the note: {log}"
                );
            }
            follow_landing(&ledger, &tag, follower, lifted);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

/// A LANDING IS NOTED HOWEVER ITS LOOP IS CUT AT ITS POINT, and a loop that
/// follows reads it as the first loop did (the review of 2026-10-20 of
/// `Session::say_point`). Two cuts that are no host step: the reader of the
/// loop's lines goes away as the landing's point line prints (a CLI `aterm
/// supervise watch` piped into a reader that quit, [`Cut::ReaderGone`]), and
/// the host hangs the loop's connection up as the fold publishes the end
/// (the window's host stopping a worker at once, [`Cut::HungUp`]). Either
/// way the journal says the landing's point, then its note, before the
/// loop's `EXIT`; and a loop that follows on those books reads it as
/// [`follow_landing`] says. Before, the note stayed owed past the failure:
/// the print that failed returned before it, and the request that failed in
/// the fold returned before the point's line too, so the books held the
/// end's ledger rows and no note — every follower started `open`, stamped
/// the end with its own clock, and (Off lifted) typed the land instruction
/// into the finished work. NEGATIVE CONTROL: the uncut first loops of
/// [`a_tasks_end_the_hosts_step_took_is_noted_and_its_follower_types_nothing`].
#[test]
fn a_tasks_end_is_noted_when_its_point_line_or_its_publish_fails() {
    for cut in [Cut::ReaderGone, Cut::HungUp] {
        for (follower, lifted) in [("land", false), ("land", true), ("cruise", true)] {
            let tag = format!("end-cut-{cut:?}-{follower}-{lifted}");
            let (dir, ledger) = files(&tag, &[], &[]);
            let (told, turns, lines) = ended_first_cut(&ledger, LANDING_REPLY, cut);
            let first = journal_lines(&ledger);
            let log = format!("{tag}: {first:#?}\n{lines:#?}");
            assert_eq!(turns, 1, "the land instruction alone: {log}");
            assert_eq!(told, [TaskFace::Open, TaskFace::Landed], "{log}");
            let end = first
                .iter()
                .position(|l| l.starts_with("DONE ") && l.contains(&format!(" rule={RULE_LAND} ")))
                .unwrap_or_else(|| panic!("the landing's note: {log}"));
            assert!(
                end > 0 && first[end - 1].starts_with("EVENT idle "),
                "the landing's point, then its note: {log}"
            );
            assert!(
                first[end + 1..].iter().any(|l| l.starts_with("EXIT ")),
                "the loop's end after the note: {log}"
            );
            follow_landing(&ledger, &tag, follower, lifted);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
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
    m.history_served();
    m.autopilot = Some(word);
    m.vanish_after = Some(30);
    m.cursor_on_caret = true;
    m.stall_sleep = Some(Duration::from_millis(20));
    let journal = crate::supervise::approvals::journal_beside(ledger);
    let opts = SuperviseOpts {
        policy: SupervisorConfig {
            human_grace_s: 0,
            ..owners_policy()
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

/// A RELAUNCHED TASK LANDED UNDER LAND GETS NO CARRY-ON (the Autopilot
/// design's §9.4.8; FOREVER LAND: only Land's landing is done): a restart
/// of the same session whose landing is seeded (its journal's `DONE …
/// rule=land@v1`) is told done under Land at its first point; the host
/// types nothing (`HOST … carry-on step=done:said-done`), its reply reads
/// landed — noted right after the point's own line and before the host's
/// (`Session::say_point`) — and the face is landed. NEGATIVE CONTROLS: a cold restore's new
/// tab — its journal and ledger empty — whose resumed conversation's last
/// reply is a defense is told no such thing (no landing of this loop's or
/// seeded: the words alone are a claim), nor is a Land session mid-work, or
/// a Cruise session whose reply is a defense; the host's carry-on is its to
/// decide as before.
#[test]
fn a_relaunched_task_landed_under_land_is_told_so_and_typed_nothing() {
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
    // A restart of the same session, its landing seeded: told so, landed.
    let (points, faces, turns, notes, lines) = carried("carry-landed", LANDING_REPLY, "land", true);
    assert_eq!(points, [done], "{notes:#?}");
    assert_eq!(turns, 0, "nothing typed: {lines:#?}");
    assert_eq!(faces, [TaskFace::Landed], "{notes:#?}");
    let host_line = notes
        .iter()
        .position(|l| l.starts_with("HOST ") && l.ends_with("carry-on step=done:said-done"))
        .unwrap_or_else(|| panic!("the host's word: {notes:#?}"));
    // The landing restated at the point is noted right after the point's
    // own line and before the host's (`Session::say_point`).
    let end = notes
        .iter()
        .rposition(|l| l.starts_with("DONE ") && l.contains(&format!(" rule={RULE_LAND} ")))
        .unwrap_or_else(|| panic!("the landing's note: {notes:#?}"));
    assert!(
        end > 0 && notes[end - 1].starts_with("EVENT idle ") && end < host_line,
        "the point, its landing, then the host's word: {notes:#?}"
    );
    // NEGATIVE CONTROLS: a cold restore's defense, mid-work under Land, a
    // defense under Cruise.
    for (tag, said, word) in [
        ("carry-cold", LANDING_REPLY, "land"),
        ("carry-mid-work", PROGRESS, "land"),
        ("carry-cruise", LANDING_REPLY, "cruise"),
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
/// CONTROL: the landing lands (its clean defense), the banner comes after it,
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
        m.history_served();
        m.autopilot = Some("land");
        m.turn_gates = vec![1];
        m.vanish_after = Some(30);
        m.cursor_on_caret = true;
        m.stall_sleep = Some(Duration::from_millis(20));
        let opts = SuperviseOpts {
            policy: SupervisorConfig {
                human_grace_s: 0,
                ..owners_policy()
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
    let (points, restarts, notes, lines) = restarted(
        "carry-landed-then-banner",
        vec![ended(LANDING_REPLY)],
        LANDING_REPLY,
    );
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

/// A LOOP STARTED UNDER CRUISE OVER A LANDING'S BOOKS TELLS NO LANDING (the
/// merge review's P2): the journal holds a real landing (`DONE …
/// rule=land@v1`), and the session is now under Cruise — started fresh, or
/// as its Off lifts. The loop reads the session's word before it tells its
/// start face ([`TurnEndState::start_under`]), so it tells `open` and never
/// `landed`: told `landed` first, the window showed `status task=landed`,
/// the glass's end mark and the chip's `· landed` for a Cruise task until
/// its first point. NEGATIVE CONTROL: the same books under Land restate the
/// landing, with its note's time, and type nothing.
#[test]
fn a_cruise_loop_over_a_landings_books_tells_no_landing() {
    let (dir, ledger) = files("cruise-over-landing", &[], &[]);
    let (told, turns, lines) = ended_first(&ledger, LANDING_REPLY, false);
    assert_eq!(turns, 1, "{lines:#?}");
    assert_eq!(told, [TaskFace::Open, TaskFace::Landed], "{lines:#?}");
    let at = note_time(&ledger, RULE_LAND);
    for lifted in [false, true] {
        let host = Arc::new(Faces {
            lifted,
            ..Faces::default()
        });
        let (lines, _) = run(
            vec![ended(LANDING_REPLY)],
            "cruise",
            Vec::new(),
            &host,
            Some(&ledger),
            None,
        );
        assert_eq!(
            host.told().first(),
            Some(&TaskFace::Open),
            "lifted={lifted}: {lines:#?}"
        );
        assert!(
            !host.told().contains(&TaskFace::Landed),
            "lifted={lifted}: told {:?}: {lines:#?}",
            host.told()
        );
        assert_eq!(host.noted(), [], "lifted={lifted}: {lines:#?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
    // NEGATIVE CONTROL: under Land.
    let (dir, ledger) = files("land-over-landing", &[], &[]);
    let (told, _, lines) = ended_first(&ledger, LANDING_REPLY, false);
    assert_eq!(told, [TaskFace::Open, TaskFace::Landed], "{lines:#?}");
    let at_land = note_time(&ledger, RULE_LAND);
    let host = Arc::new(Faces::default());
    let (lines, turns) = run(
        vec![ended(LANDING_REPLY)],
        "land",
        Vec::new(),
        &host,
        Some(&ledger),
        None,
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(turns, 0, "{lines:#?}");
    assert_eq!(host.told(), [TaskFace::Landed], "{lines:#?}");
    assert_eq!(host.noted(), [(TaskFace::Landed, at_land)], "{lines:#?}");
    assert!(at > 0);
}

/// A LOOP THAT ATTACHES TO THE LANDED TASK'S OWN WAKE OVER WORK IN THE
/// REPOSITORY KEEPS NO LANDING (`TurnEndState::wake_start_owed`, the Off's
/// rule of `TurnEndState::observe`): the loop's first read is the wake
/// running, so no probe of its own saw the wake begin, and what the probe
/// finds at the wake's end may be the wake's work — or, where the loop
/// started as the session's Off lifted (a person's act), the person's own
/// edit made under Off. The review of b8/residuals found a successor take
/// its first live frame's probe as the wake's start (the wake forgiven the
/// work), and an Off and back then the wake come back landed over the
/// person's edit. Both now reopen: Land's nudge is typed. NEGATIVE CONTROL:
/// the same wakes over a clean repository keep the landing, nothing typed.
#[test]
fn a_loop_attached_to_the_tasks_own_wake_keeps_no_landing_over_work_found() {
    let min = |n: u64| Duration::from_secs(60 * n);
    let mark = said_mark(Some(LANDING_REPLY));
    let dirty = crate::harness::repo_work::RepoWork {
        repo: "/Users//a/Developer/parser".to_string(),
        changed: 2,
        paths: vec!["src/lexer.rs".to_string()],
        state: Some(0x5eed),
        ..Default::default()
    };
    let wake = "The monitor fired: the deploy finished.";
    let go = |tag: &str, lifted: bool, repo: Option<crate::harness::repo_work::RepoWork>| {
        let (dir, path) = files(
            tag,
            &[ledger_row(RULE_LAND, "land", min(10))],
            &[
                marked_row("EVENT idle seq=104 landed", min(9), None, mark),
                journal_row(LANDED_NOTE, min(9), None),
            ],
        );
        let host = Arc::new(Faces {
            repo,
            finished: Some(true),
            lifted,
            ..Faces::default()
        });
        let (lines, turns) = run(
            vec![busy_screen(), ended(wake)],
            "land",
            Vec::new(),
            &host,
            Some(&path),
            None,
        );
        let _ = std::fs::remove_dir_all(&dir);
        (turns, host.told(), lines)
    };
    for lifted in [false, true] {
        let (turns, told, lines) = go(
            &format!("attach-dirty-{lifted}"),
            lifted,
            Some(dirty.clone()),
        );
        assert_eq!(turns, 1, "lifted={lifted}: {lines:#?}");
        assert_eq!(
            told.last(),
            Some(&TaskFace::Open),
            "lifted={lifted}: {told:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains(&format!("rule={RULE_LAND_NUDGE} "))
                    || l.contains(&format!("rule={RULE_LAND} "))),
            "lifted={lifted}: {lines:#?}"
        );
        // NEGATIVE CONTROL: a clean repository.
        let (turns, told, lines) = go(&format!("attach-clean-{lifted}"), lifted, None);
        assert_eq!(turns, 0, "lifted={lifted}: {lines:#?}");
        assert_eq!(
            told.last(),
            Some(&TaskFace::Landed),
            "lifted={lifted}: {told:?}"
        );
    }
}

/// A LANDED TASK'S TALL WAKE WITH A NEED ABOVE THE SCREEN KEEPS NO LANDING
/// (`TurnEndState::reads_turn_whole`): a successor's first read is the
/// landed task's own wake (the record saying nobody asked), a turn whose
/// start — a `NEEDS YOU:` said there — is in the history above the screen,
/// its last words claiming no done. The review of b8/residuals found the
/// history read only where the last words claimed done: the landing held
/// over the need, which was never raised. Now the turn is read whole: the
/// need is put up and the landing reopens. NEGATIVE CONTROLS: the same
/// wake short enough to fit does the same, and a tall wake with no need in
/// it keeps the landing, nothing typed.
#[test]
fn a_tall_wake_with_a_need_above_the_screen_keeps_no_landing() {
    let min = |n: u64| Duration::from_secs(60 * n);
    let mark = said_mark(Some(LANDING_REPLY));
    let wake_of = |calls: usize, need: bool| -> (Vec<String>, Vec<String>) {
        let mut drawn: Vec<String> = vec![
            "⏺ Background command \"deploy\" completed (exit code 0)".to_string(),
            String::new(),
            "⏺ The deploy finished.".to_string(),
        ];
        if need {
            drawn.push("  NEEDS YOU: the signing credential expired".to_string());
        }
        for call in 0..calls {
            drawn.extend([
                String::new(),
                format!("⏺ Bash(curl -s https://status/{call})"),
                format!("  ⎿  ok {call}"),
            ]);
        }
        drawn.extend(rows(&[
            "",
            "⏺ The monitor fired: the deploy finished.",
            "",
            "✻ Worked for 3m 2s · done 4:24 PM",
            "",
        ]));
        drawn.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
        if drawn.len() > 30 {
            let screen = drawn.split_off(drawn.len() - 30);
            (screen, drawn)
        } else {
            (drawn, Vec::new())
        }
    };
    let go = |calls: usize, need: bool| {
        let (screen, above) = wake_of(calls, need);
        let tall = !above.is_empty();
        let (dir, path) = files(
            &format!("tall-wake-{calls}-{need}"),
            &[ledger_row(RULE_LAND, "land", min(10))],
            &[
                marked_row("EVENT idle seq=104 landed", min(9), None, mark),
                journal_row(LANDED_NOTE, min(9), None),
            ],
        );
        let host = Arc::new(Faces {
            finished: Some(true),
            ..Faces::default()
        });
        let (lines, turns, m) = run_kept(
            vec![busy_screen(), screen],
            "land",
            Vec::new(),
            &host,
            Some(&path),
            None,
            |m| {
                m.scrollback = Some(above);
                m.alt_screen = Some(false);
            },
        );
        let _ = std::fs::remove_dir_all(&dir);
        let raised = m.needs_calls.iter().any(|c| {
            c.starts_with("set attention owner=needs ") && c.contains("the signing credential")
        });
        (tall, turns, host.told(), raised, lines)
    };
    for calls in [20, 1] {
        let (tall, turns, told, raised, lines) = go(calls, true);
        assert_eq!(tall, calls > 1);
        assert!(raised, "tall={tall}: {lines:#?}");
        assert_eq!(turns, 1, "tall={tall}: {lines:#?}");
        assert_eq!(told.last(), Some(&TaskFace::Open), "tall={tall}: {told:?}");
    }
    // NEGATIVE CONTROL: a tall wake that names no need.
    let (tall, turns, told, raised, lines) = go(20, false);
    assert!(tall);
    assert!(!raised, "{lines:#?}");
    assert_eq!(turns, 0, "{lines:#?}");
    assert_eq!(told.last(), Some(&TaskFace::Landed), "{told:?}");
}

/// A RESTART AFTER A KEPT WAKE KEEPS THE LANDING
/// (`TurnEndState::take_wake_restated`, `TurnEndState::seed_landed_by_wake`):
/// a live landing, then the landed task's own wake (nobody's act), kept
/// landed; then the loop restarts (nobody's act) over the wake's screen.
/// The review of b8/residuals found the restart read the wake's turn-ran row
/// and its point (the wake's own words, no defense) as a turn after the
/// landing: the seed was dropped and Land's nudge typed into the landed
/// work. The loop now restates the landing's note after the wake's point,
/// and the restart takes it up — and a second restart, over the note the
/// first restated, keeps it too, as does the loop after a successor that
/// attached as a later wake ran and kept it. NEGATIVE CONTROLS: a restart with no wake
/// keeps it as before, and a restart over other last words (a turn no loop
/// watched after the wake) drops it.
#[test]
fn a_restart_after_a_kept_wake_keeps_the_landing() {
    let wake = "The monitor fired: the deploy finished.";
    let restart = |ledger: &Path, last: &str| {
        let host = Arc::new(Faces {
            finished: Some(true),
            ..Faces::default()
        });
        let (lines, turns) = run(
            vec![ended(last)],
            "land",
            Vec::new(),
            &host,
            Some(ledger),
            None,
        );
        (turns, host.told(), lines)
    };
    for with_wake in [true, false] {
        let (dir, ledger) = files(&format!("kept-wake-{with_wake}"), &[], &[]);
        let host = Arc::new(Faces {
            finished: Some(true),
            ..Faces::default()
        });
        let mut screens = vec![
            busy_screen(),
            ended(PROGRESS),
            busy_screen(),
            ended(LANDING_REPLY),
        ];
        if with_wake {
            screens.extend([busy_screen(), ended(wake)]);
        }
        let (lines, turns) = run(screens, "land", vec![1], &host, Some(&ledger), None);
        assert_eq!(turns, 1, "{lines:#?}");
        assert_eq!(
            host.told(),
            [TaskFace::Open, TaskFace::Landed],
            "{lines:#?}"
        );
        let last = if with_wake { wake } else { LANDING_REPLY };
        for again in 0..2 {
            let (turns, told, lines) = restart(&ledger, last);
            assert_eq!(
                turns, 0,
                "with_wake={with_wake} restart {again}: {lines:#?}"
            );
            assert_eq!(
                told,
                [TaskFace::Landed],
                "with_wake={with_wake} restart {again}"
            );
        }
        // A successor that attaches as the next wake runs keeps the landing
        // through it, and restates it so: the loop after it keeps it too.
        let later = "The second monitor fired: the backup finished.";
        let host = Arc::new(Faces {
            finished: Some(true),
            ..Faces::default()
        });
        let (lines, turns) = run(
            vec![busy_screen(), ended(later)],
            "land",
            Vec::new(),
            &host,
            Some(&ledger),
            None,
        );
        assert_eq!(turns, 0, "with_wake={with_wake}: {lines:#?}");
        assert_eq!(host.told().last(), Some(&TaskFace::Landed), "{lines:#?}");
        let (turns, told, lines) = restart(&ledger, later);
        assert_eq!(
            turns, 0,
            "with_wake={with_wake} after the successor's wake: {lines:#?}"
        );
        assert_eq!(told, [TaskFace::Landed], "with_wake={with_wake}");
        // NEGATIVE CONTROL: other last words since.
        let (_, told, lines) = restart(&ledger, "Rebased the docs branch onto main.");
        assert_ne!(told.last(), Some(&TaskFace::Landed), "{lines:#?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A SWITCH FROM LAND TO CRUISE ON THE LANDED SCREEN IS READ AT THE WAKE'S
/// END (`Session::turn_end_at_point`'s fresh word): the loop idles on the
/// landed screen and reads no word there, so the owner's switch to Cruise
/// was unread when the landed task's own wake ended, and the wake kept the
/// landing under Cruise — nothing went on (the review of b8/residuals). The
/// word is read fresh where a turn ends over a landing: under Cruise the
/// task reopens and Cruise's continuation is typed. NEGATIVE CONTROL: with
/// no switch (Land) the same wake keeps the landing, nothing typed after
/// the land instruction.
#[test]
fn a_switch_to_cruise_on_the_landed_screen_is_read_at_the_wakes_end() {
    let wake = "The monitor fired: the deploy finished.";
    let go = |switch: bool| {
        let (dir, ledger) = files(&format!("cruise-at-wake-{switch}"), &[], &[]);
        let host = Arc::new(Faces {
            finished: Some(true),
            ..Faces::default()
        });
        let screens = vec![
            busy_screen(),
            ended(PROGRESS),
            busy_screen(),
            ended(LANDING_REPLY),
            busy_screen(),
            ended(wake),
            ended(wake),
        ];
        let (lines, turns) = run_on(screens, "land", vec![1], &host, Some(&ledger), None, |m| {
            if switch {
                m.autopilot_on.insert(3, Some("cruise"));
            }
            m.vanish_after = Some(12);
        });
        let _ = std::fs::remove_dir_all(&dir);
        let typed: Vec<String> = lines
            .iter()
            .filter(|l| l.starts_with("CONTINUED "))
            .cloned()
            .collect();
        (turns, host.told(), typed, lines)
    };
    let (turns, told, typed, lines) = go(true);
    assert_eq!(turns, 2, "{lines:#?}");
    assert_eq!(
        told,
        [TaskFace::Open, TaskFace::Landed, TaskFace::Open],
        "{lines:#?}"
    );
    assert!(
        typed
            .last()
            .is_some_and(|l| l.contains(" rule=continue@v1 ")),
        "{typed:#?}"
    );
    // NEGATIVE CONTROL: no switch.
    let (turns, told, typed, lines) = go(false);
    assert_eq!(turns, 1, "{lines:#?}");
    assert_eq!(told, [TaskFace::Open, TaskFace::Landed], "{lines:#?}");
    assert_eq!(typed.len(), 1, "{typed:#?}");
}

/// What a client says where nothing serves the control socket yet: a
/// seamless update's successor starts its loops a few milliseconds before
/// its socket listens, so their first bare `meta` is not served.
const UNSERVED: &str = "connect /d/aterm-7.sock: Connection refused (os error 61) — aterm \
                        isn't running (nothing is serving this control socket); launch \
                        aterm.app (`open -a aterm`), then `aterm ctl instances` names its pid";

/// The bare `meta` requests a run made, each with whether it was served.
fn bare_metas(m: &Mock) -> Vec<bool> {
    m.order
        .iter()
        .filter(|(l, _)| l == "meta")
        .map(|(_, ok)| *ok)
        .collect()
}

/// A LANDING RESTARTED BY A LOOP WHOSE START-UP `meta` WAS NOT SERVED STILL
/// READS LANDED under the session's own Land (the final check of
/// 2026-10-20, W1: a seamless update's successor, its loops started before
/// its control socket listened). The table says Cruise; the session says
/// Land. The word is unread, not absent: the seed is not decided under the
/// table's word at the start, and the first point reads the word before it
/// folds, so the landing is taken up — told `landed`, nothing typed, no
/// done check — and the word is read again once served. NEGATIVE CONTROL:
/// the same restart with the start-up `meta` served reads the same; with the
/// read gated on the host keeping a word (head before the fix) the lost read
/// left the table's Cruise deciding — told `open`, the done check typed
/// (`continue-done-check@v1`), and no second `meta` read.
#[test]
fn a_landing_whose_successors_first_meta_is_unserved_still_reads_landed() {
    for lose in [false, true] {
        let (dir, ledger) = files(&format!("unserved-meta-{lose}"), &[], &[]);
        let (told, turns, _) = land_first(&ledger, Vec::new(), &[]);
        assert_eq!(turns, 1, "lose={lose}: the land instruction alone");
        assert_eq!(told, [TaskFace::Open, TaskFace::Landed], "lose={lose}");
        let (told, turns, added, lines, m) =
            after_off_on(&ledger, "land", vec![ended(LANDING_REPLY)], |m| {
                if lose {
                    m.verb_replies
                        .insert("meta", VecDeque::from([failed(1, UNSERVED)]));
                }
            });
        let metas = bare_metas(&m);
        let log = format!("lose={lose} {metas:?}\n{added:#?}\n{lines:#?}");
        assert_eq!(told, [TaskFace::Landed], "{log}");
        assert_eq!(turns, 0, "nothing typed into landed work: {log}");
        assert!(
            !lines
                .iter()
                .any(|l| l.contains(&format!("rule={RULE_DONE_CHECK} "))),
            "{log}"
        );
        assert!(added.iter().any(|l| landed_note(l)), "{log}");
        assert!(metas.contains(&true), "the word read once served: {log}");
        if lose {
            assert_eq!(metas.first(), Some(&false), "{log}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A SESSION'S OWN LAND UNDER A CRUISE TABLE, ITS LOOP'S START-UP `meta`
/// UNSERVED, GETS LAND'S INSTRUCTION at its first point, never Cruise's
/// continuation: the word never read is read where the decision would act
/// (`Session::autopilot_read`), not left to the table's because no reply
/// ever said the host keeps one (W1's second case). NEGATIVE CONTROL: the
/// same run with the start-up `meta` served types the same `land@v1`; head
/// before the fix typed the table's Cruise continuation instead.
#[test]
fn an_unread_word_is_read_at_the_first_decision_not_left_to_the_table() {
    for lose in [false, true] {
        let host = Arc::new(Faces::default());
        let (lines, turns, m) = run_kept(
            vec![busy_screen(), ended(PROGRESS)],
            "land",
            vec![1],
            &host,
            None,
            None,
            |m| {
                if lose {
                    m.verb_replies
                        .insert("meta", VecDeque::from([failed(1, UNSERVED)]));
                }
            },
        );
        let log = format!("lose={lose} {:?}\n{lines:#?}", bare_metas(&m));
        assert_eq!(turns, 1, "{log}");
        assert!(
            lines
                .iter()
                .any(|l| l.contains(&format!("rule={RULE_LAND} "))),
            "Land's instruction: {log}"
        );
        assert!(
            !lines
                .iter()
                .any(|l| l.starts_with("CONTINUED ") && !l.contains(&format!(" rule={RULE_LAND} "))),
            "no Cruise continuation: {log}"
        );
    }
}

/// A RESTART OVER A LANDING'S BOOKS under the session's word `word` and the
/// table's `table`, its first bare `meta` lost where `lose` (a seamless
/// update's successor, its socket not listening yet), the host as `lifted`
/// says it started the loop, the mock set up further by `more`: what the
/// host was told, what the loop said, the turns typed, the bare `meta`s
/// read, and the lines it added to the journal.
fn restart_over_landing(
    ledger: &Path,
    screens: Vec<Vec<String>>,
    word: &'static str,
    table: Autopilot,
    lifted: bool,
    lose: bool,
    more: impl FnOnce(&mut Mock),
) -> (Vec<TaskFace>, Vec<String>, usize, Vec<bool>, Vec<String>) {
    let before = journal_lines(ledger).len();
    let host = Arc::new(Faces {
        lifted,
        ..Faces::default()
    });
    let mut m = Mock::new(screens);
    m.autopilot = Some(word);
    m.turn_gates = Vec::new();
    m.vanish_after = Some(6);
    m.stall_sleep = Some(Duration::from_millis(20));
    m.history_served();
    if lose {
        m.verb_replies
            .insert("meta", VecDeque::from([failed(1, UNSERVED)]));
    }
    more(&mut m);
    let opts = SuperviseOpts {
        policy: SupervisorConfig {
            human_grace_s: 0,
            autopilot: table,
            ..owners_policy()
        },
        journal: Some(crate::supervise::approvals::journal_beside(ledger)),
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
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
    let turns = count(&m, "turn");
    let added = journal_lines(ledger).split_off(before);
    (host.told(), lines, turns, bare_metas(&m), added)
}

/// AN OFF SESSION'S LANDING SURVIVES A LOST START-UP `meta`, and lands
/// again when the owner turns it to Land (the second review of 2026-10-20,
/// R1). The session's word is `off`, the table's Cruise; the word turns
/// `land` at the third read served. Served at the start, the loop keeps
/// the seed under Off (`TurnEndState::start_under`), parks, and at the
/// landing's point under Land notes `DONE … rule=land@v1` — told `landed`
/// alone, nothing typed. Lost at the start, the word is read at the first
/// look, before its first decision (the Off park among them), so the run
/// reads the same. NEGATIVE CONTROL: the served run; and the branch with the
/// word read only at the point (the first fix of W1) passed the Off park
/// under the table's Cruise, folded the point under Off — dropping the
/// seed — and was told `[Landed, Open]` and typed `land@v1` into landed work.
#[test]
fn an_off_landing_whose_first_meta_is_lost_reads_as_if_it_were_served() {
    for lose in [false, true] {
        let (dir, ledger) = files(&format!("off-land-lost-{lose}"), &[], &[]);
        let (told, turns, _) = land_first(&ledger, Vec::new(), &[]);
        assert_eq!(
            (told.as_slice(), turns),
            ([TaskFace::Open, TaskFace::Landed].as_slice(), 1)
        );
        let (told, lines, turns, metas, added) = restart_over_landing(
            &ledger,
            vec![ended(LANDING_REPLY)],
            "off",
            Autopilot::Cruise,
            false,
            lose,
            |m| {
                m.autopilot_at.push((3, Some("land")));
                m.vanish_after = Some(12);
            },
        );
        let log = format!("lose={lose} {metas:?}\n{added:#?}\n{lines:#?}");
        assert_eq!(told, [TaskFace::Landed], "{log}");
        assert_eq!(turns, 0, "nothing typed into landed work: {log}");
        assert!(added.iter().any(|l| l.starts_with("PARKED ")), "{log}");
        assert!(added.iter().any(|l| landed_note(l)), "{log}");
        assert_eq!(metas.first(), Some(&!lose), "{log}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A RESTART OVER A LANDING'S BOOKS WHOSE START-UP `meta` WAS LOST TELLS NO
/// FACE BEFORE THE WORD IS READ (the merge review's P2, its lost-read case;
/// the second review of 2026-10-20, R1). Each word under each table, the
/// loop started fresh or as Off lifts: lost, the face waits for the first
/// look's read and is told what the served start tells — `open` alone under
/// Cruise (never `landed` first: the window read `status task=landed` and
/// drew the end mark for a Cruise task until its first point), `landed`
/// alone under Land and Off. NEGATIVE CONTROL: the served start, told the
/// same; the branch that told the seeded face before the read was told
/// `[Landed, Open]` under Cruise and under Off.
#[test]
fn a_restart_whose_first_meta_is_lost_tells_the_face_its_served_start_tells() {
    for (word, table, want) in [
        ("cruise", Autopilot::Cruise, TaskFace::Open),
        ("cruise", Autopilot::Land, TaskFace::Open),
        ("land", Autopilot::Cruise, TaskFace::Landed),
        ("off", Autopilot::Cruise, TaskFace::Landed),
    ] {
        for lifted in [false, true] {
            for lose in [false, true] {
                let tag = format!("lost-face-{word}-{table:?}-{lifted}-{lose}");
                let (dir, ledger) = files(&tag, &[], &[]);
                let (told, turns, _) = land_first(&ledger, Vec::new(), &[]);
                assert_eq!(
                    (told.as_slice(), turns),
                    ([TaskFace::Open, TaskFace::Landed].as_slice(), 1)
                );
                let (told, lines, _, metas, _) = restart_over_landing(
                    &ledger,
                    vec![ended(LANDING_REPLY)],
                    word,
                    table,
                    lifted,
                    lose,
                    |_| {},
                );
                let log = format!("{tag} {metas:?}\n{lines:#?}");
                assert_eq!(told, [want], "{log}");
                assert_eq!(metas.first(), Some(&!lose), "{log}");
                let _ = std::fs::remove_dir_all(&dir);
            }
        }
    }
}
