// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::harness::acts::TaskFace;
use crate::supervise::policy::turn_end::{
    RULE_DONE_CHECK, RULE_LAND, RULE_LAND_CHECK, RULE_LAND_NUDGE, RULE_LEFT,
};

const LANDING_REPLY: &str = "Committed and pushed the parser and its tests.\nNothing left: all of \
                             it is merged into the remote main branch.";
const PROGRESS: &str = "Committing the parser now; the lexer is next.";
/// The land check's one landing answer: a landing's last reply.
const LANDED: &str = crate::supervise::policy::land_check::LANDED;

/// A LANDING'S LAST SCREENS: the defense (where the land check is typed, so a run gates it on
/// the check's turn), the check's turn, and its `LANDED`.
fn landing() -> [Vec<String>; 3] {
    [ended(LANDING_REPLY), busy_screen(), ended(LANDED)]
}

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
    fn repo_work(&self) -> Option<crate::harness::repo_work::RepoRead> {
        self.probes
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Some(crate::harness::repo_work::RepoRead::of_found(
            self.repos
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pop_front()
                .unwrap_or_else(|| self.repo.clone()),
        ))
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

/// A turn of work: a call it ran, then its words.
fn ended(said: &str) -> Vec<String> {
    ended_after(said, "3m 2s")
}

/// [`ended`] after `worked` of the turn's (`12s`: a short turn, no real work).
fn ended_after(said: &str, worked: &str) -> Vec<String> {
    let mut lines: Vec<String> = said.lines().map(|l| format!("⏺ {l}")).collect();
    for l in lines.iter_mut().skip(1) {
        *l = l.replacen("⏺ ", "  ", 1);
    }
    lines.splice(
        0..0,
        [
            "⏺ Bash(git push origin main)".to_string(),
            "  ⎿  main -> main".to_string(),
            String::new(),
        ],
    );
    lines.extend([
        String::new(),
        format!("✻ Worked for {worked} · done 4:24 PM"),
        String::new(),
    ]);
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    let mut r = rows(&refs);
    r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
    r
}

fn answered(prompt: &str, said: &str) -> Vec<String> {
    let mut r = rows(&[&format!("❯ {prompt}"), ""]);
    r.extend(ended(said));
    r
}

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

fn run_kept(
    screens: Vec<Vec<String>>,
    word: &'static str,
    gates: Vec<usize>,
    host: &Arc<Faces>,
    ledger: Option<&Path>,
    human: Option<&'static str>,
    more: impl FnOnce(&mut Mock),
) -> (Vec<String>, usize, Mock) {
    run_kept_tracked(screens, word, gates, host, ledger, human, None, more)
}

/// [`run_kept`] of a loop that tracks a task record under `tasks`.
#[expect(
    clippy::too_many_arguments,
    reason = "run_kept's own list and the one thing it leaves out"
)]
fn run_kept_tracked(
    screens: Vec<Vec<String>>,
    word: &'static str,
    gates: Vec<usize>,
    host: &Arc<Faces>,
    ledger: Option<&Path>,
    human: Option<&'static str>,
    tasks: Option<PathBuf>,
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
        tasks,
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

#[test]
fn a_landing_is_told_landed_once_and_someone_elses_live_turn_opens_it() {
    let landed = |then: Vec<Vec<String>>| {
        let host = Arc::new(Faces::default());
        let mut screens = vec![busy_screen(), ended(PROGRESS), busy_screen()];
        screens.extend(landing());
        screens.extend(then);
        let (lines, turns) = run(screens, "land", vec![1, 3], &host, None, None);
        assert_eq!(turns, 2, "the instruction, then the land check: {lines:#?}");
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
    assert_eq!(landed(Vec::new()), [TaskFace::Open, TaskFace::Landed]);
}

#[test]
fn a_landed_tasks_own_wake_tells_no_face() {
    let wake = |finished: Option<bool>| {
        let host = Arc::new(Faces {
            finished,
            ..Faces::default()
        });
        let mut screens = vec![busy_screen(), ended(PROGRESS), busy_screen()];
        screens.extend(landing());
        screens.extend([
            busy_screen(),
            ended("The monitor fired: the deploy finished."),
        ]);
        let (lines, turns) = run(screens, "land", vec![1, 3], &host, None, None);
        (host.told(), turns, lines)
    };
    let (told, turns, lines) = wake(Some(true));
    assert_eq!(told, [TaskFace::Open, TaskFace::Landed], "{lines:#?}");
    assert_eq!(turns, 2, "the instruction and the check: {lines:#?}");
    let (told, _, lines) = wake(None);
    assert_eq!(
        told.get(..3),
        Some(&[TaskFace::Open, TaskFace::Landed, TaskFace::Open][..]),
        "{lines:#?}"
    );
}

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
            repos: Mutex::new([None, None, None, began].into_iter().collect()),
            ..Faces::default()
        });
        let mut screens = vec![busy_screen(), ended(PROGRESS), busy_screen()];
        screens.extend(landing());
        screens.extend([
            busy_screen(),
            ended("The monitor fired: the deploy finished."),
        ]);
        let (lines, turns) = run(screens, "land", vec![1, 3], &host, None, None);
        (host.told(), turns, lines)
    };
    let (told, turns, lines) = wake(Some(edit.clone()));
    assert_eq!(turns, 2, "the instruction and the check: {lines:#?}");
    assert_eq!(told, [TaskFace::Open, TaskFace::Landed], "{lines:#?}");
    let (told, turns, lines) = wake(None);
    assert_eq!(turns, 3, "the nudge: {lines:#?}");
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
    let (told, turns, lines) = restart("restart-wake-unknown", None);
    assert_eq!(turns, 1, "Land's act: {lines:#?}");
    assert_eq!(told, [TaskFace::Landed, TaskFace::Open], "{lines:#?}");
}

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

#[test]
fn a_landing_goes_on_past_two_nudges_and_is_told_landed_only_on_its_defense() {
    let land_on = |last: &[Vec<String>], gates: Vec<usize>| {
        let host = Arc::new(Faces::default());
        let mut screens = vec![
            busy_screen(),
            ended(PROGRESS),
            busy_screen(),
            ended(PROGRESS),
            busy_screen(),
            ended(PROGRESS),
            busy_screen(),
            ended(PROGRESS),
            busy_screen(),
        ];
        screens.extend(last.iter().cloned());
        screens.push(busy_screen());
        let (lines, turns) = run(screens, "land", gates, &host, None, None);
        (turns, host.told(), lines)
    };
    let (turns, told, lines) = land_on(&[ended(PROGRESS)], vec![1, 3, 5, 7]);
    assert_eq!(turns, 5, "the instruction and four nudges: {lines:#?}");
    assert_eq!(told, [TaskFace::Open], "{lines:#?}");
    let (turns, told, lines) = land_on(&landing(), vec![1, 3, 5, 7, 9]);
    assert_eq!(
        turns, 5,
        "the instruction, three nudges and the check: {lines:#?}"
    );
    assert_eq!(
        told,
        [TaskFace::Open, TaskFace::Landed, TaskFace::Open],
        "{lines:#?}"
    );
}

#[test]
fn a_restart_of_a_landing_tells_landed_and_types_nothing() {
    for (end, last, face, gates) in [
        (
            landing().to_vec(),
            LANDED,
            TaskFace::Landed,
            vec![1, 3, 5, 7],
        ),
        (
            vec![ended(PROGRESS)],
            PROGRESS,
            TaskFace::Open,
            vec![1, 3, 5],
        ),
    ] {
        let (dir, path) = files("restart-landed", &[], &[]);
        let host = Arc::new(Faces::default());
        let mut screens = vec![
            busy_screen(),
            ended(PROGRESS),
            busy_screen(),
            ended(PROGRESS),
            busy_screen(),
            ended(PROGRESS),
            busy_screen(),
        ];
        screens.extend(end);
        let (lines, turns) = run(screens, "land", gates, &host, Some(&path), None);
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

#[test]
fn the_loops_start_is_told_once() {
    let host = Arc::new(Faces::default());
    let (_, turns) = run(vec![ended(PROGRESS)], "off", Vec::new(), &host, None, None);
    assert_eq!(turns, 0);
    assert_eq!(host.told(), [TaskFace::Open]);
}

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

fn journal_row(line: &str, ago: Duration, sid: Option<&str>) -> String {
    marked_row(line, ago, sid, None)
}

fn marked_row(line: &str, ago: Duration, sid: Option<&str>, mark: Option<u64>) -> String {
    let at = crate::supervise::journal::unix_ms() - i64::try_from(ago.as_millis()).unwrap();
    let mut rec = crate::supervise::journal::JournalRecord::of_line(at, sid, line, None);
    rec.mark = mark;
    rec.to_json()
}

const LANDED_NOTE: &str = "DONE seq=104 rule=land@v1 the worker said it is done after the land \
     instruction (autopilot land): nothing more is typed until a person or an orchestrator types";

/// What a loop journals at a worker's own failed conclusion.
const FAILED_NOTE: &str = "FAILED seq=104 rule=land@v1 the worker said it could not finish \
     (autopilot land): nothing more is typed until a person or an orchestrator types";

/// The reply of a worker that gave up.
const COULD_NOT: &str = "Could not finish: the grammar the parser needs was deleted upstream.";

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

#[test]
fn an_old_landing_never_survives_a_later_box_with_the_same_sequence() {
    const POINT: &str = r#"{"t":1,"sid":null,"kind":"event","phase":"idle","seq":104,"complete":null,"rows":null,"summary":"ATERM-TASK-DONE","line":"EVENT idle seq=104 ATERM-TASK-DONE","turn":null,"report":null}"#;
    const NOTE: &str = r#"{"t":2,"sid":null,"kind":"other","phase":"-","seq":null,"complete":null,"rows":null,"summary":"DONE seq=104 rule=land@v1 the worker said it is done after the land instruction","line":"DONE seq=104 rule=land@v1 the worker said it is done after the land instruction","turn":null,"report":null}"#;
    const AGAIN: &str = r#"{"t":3,"sid":null,"kind":"event","phase":"idle","seq":104,"complete":null,"rows":null,"summary":"ATERM-TASK-DONE","line":"EVENT idle seq=104 ATERM-TASK-DONE","turn":null,"report":null}"#;
    const MARKED_POINT: &str = r#"{"t":1,"sid":null,"kind":"event","phase":"idle","seq":104,"complete":null,"rows":null,"summary":"ATERM-TASK-DONE","line":"EVENT idle seq=104 ATERM-TASK-DONE","turn":null,"report":null,"mark":"feed00000000beef"}"#;
    const MARKED_AGAIN: &str = r#"{"t":3,"sid":null,"kind":"event","phase":"idle","seq":9,"complete":null,"rows":null,"summary":"ATERM-TASK-DONE","line":"EVENT idle seq=9 ATERM-TASK-DONE","turn":null,"report":null,"mark":"feed00000000beef"}"#;
    const PROMPT: &str = r#"{"t":3,"sid":null,"kind":"event","phase":"prompt","seq":104,"complete":null,"rows":null,"summary":"kind=bash classify=- command=touch c","line":"EVENT prompt seq=104 kind=bash classify=- command=touch c","turn":null,"report":null}"#;
    const APPROVED: &str = r#"{"t":3,"sid":null,"kind":"approved","phase":"prompt","seq":104,"complete":null,"rows":null,"summary":"touch c.txt","line":"APPROVED seq=104 touch c.txt","turn":null,"report":null}"#;

    for prefix in [NOTE.to_string(), format!("{POINT}\n{NOTE}")] {
        assert_eq!(landed_before(&prefix, None, Some(1)), Some(None));
        assert_eq!(
            landed_before(&format!("{prefix}\n{AGAIN}"), None, Some(1)),
            Some(None)
        );
    }
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
    let elsewhere = journal_row("EVENT idle seq=111 ⏺ Done.", min(5), Some("s-other"));
    assert!(seeded(
        "read-back-point-sid",
        std::slice::from_ref(&land_typed),
        &[note.clone(), elsewhere]
    ));
    let nudge = ledger_row("continue@v1", "keep going", min(2));
    assert!(!seeded(
        "read-back-typed",
        &[land_typed.clone(), nudge],
        std::slice::from_ref(&note)
    ));
    let theirs = journal_row(LANDED_NOTE, min(9), Some("s-other"));
    assert!(!seeded(
        "read-back-sid",
        std::slice::from_ref(&land_typed),
        &[theirs]
    ));
    assert!(!seeded("read-back-none", &[land_typed], &[]));
    assert!(landed_before("", None, None).is_none());
}

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
            let got = ended_before(&journal, None, typed.last().copied()).map(|(words, ..)| words);
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

#[test]
fn a_restated_landing_carries_the_time_its_note_was_journaled() {
    let min = |n: u64| Duration::from_secs(60 * n);
    let (dir, ledger) = files("noted-real", &[], &[]);
    let (told, turns, lines) = land_first(&ledger, Vec::new(), &[]);
    assert_eq!(
        (told, turns),
        (vec![TaskFace::Open, TaskFace::Landed], 2),
        "{lines:#?}"
    );
    let at = note_time(&ledger, RULE_LAND);
    let host = Arc::new(Faces::default());
    let (lines, turns) = run(
        vec![ended(LANDED)],
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
    let (dir, ledger) = files("noted-typed", &[], &[]);
    land_first(&ledger, Vec::new(), &[]);
    std::thread::sleep(Duration::from_millis(5));
    let mut rows = std::fs::read_to_string(&ledger).unwrap_or_default();
    rows.push_str(&ledger_row("continue@v1", "keep going", Duration::ZERO));
    rows.push('\n');
    std::fs::write(&ledger, rows).expect("ledger");
    let host = Arc::new(Faces::default());
    run(
        vec![ended(LANDED)],
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

#[test]
fn a_restated_landings_note_carries_its_first_time_forward() {
    let min = |n: u64| Duration::from_secs(60 * n);
    let (dir, ledger) = files("chain-real", &[], &[]);
    land_first(&ledger, Vec::new(), &[]);
    let first = last_done(&ledger);
    assert!(landed_note(&first.line), "{first:?}");
    assert_eq!(first.ended, None, "the first loop's own end");
    for again in 0..2 {
        std::thread::sleep(Duration::from_millis(5));
        let host = Arc::new(Faces::default());
        let (lines, turns) = run(
            vec![ended(LANDED)],
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

/// The page's reading of the files the loops wrote (`stats_windows`) counts a landing kept
/// through its wake and two restarts once, beside its one land prompt.
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
    let host = wakeful();
    let mut screens = vec![busy_screen(), ended(PROGRESS), busy_screen()];
    screens.extend(landing());
    screens.extend([busy_screen(), ended(WAKE)]);
    let (lines, turns) = run(screens, "land", vec![1, 3], &host, Some(&ledger), None);
    assert_eq!(turns, 2, "the instruction and the check: {lines:#?}");
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
    assert_eq!(read(opened), (1, 1, 1, 2), "{first:#?}");
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
        assert_eq!(read(opened), (1, 1, 1, 2), "{again}: the journal's count");
    }
    assert_eq!(notes(), 4, "{:#?}", journal_lines(&ledger));
    std::thread::sleep(Duration::from_millis(5));
    let reopened = crate::supervise::journal::unix_ms();
    let host = Arc::new(Faces::default());
    let mut screens = vec![busy_screen(), ended(PROGRESS), busy_screen()];
    screens.extend(landing());
    let (lines, turns) = run(screens, "land", vec![1, 3], &host, Some(&ledger), None);
    assert_eq!(turns, 2, "Land's act and its check: {lines:#?}");
    assert_eq!(host.told().last(), Some(&TaskFace::Landed), "{lines:#?}");
    let (today, week, share, prompts) = read(opened);
    assert_eq!(
        (today, week, share),
        (2, 2, 2),
        "{:#?}",
        journal_lines(&ledger)
    );
    assert_eq!(
        prompts, 4,
        "four land prompts: an instruction and a check per landing"
    );
    assert_eq!(read(reopened).0, 1, "{:#?}", journal_lines(&ledger));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A restart whose journal ends on the land's `DONE`, no typed row after it and the screen's
/// reply still a defense, is told `landed` at its start and nothing after (never a move from
/// open, which the window would record as a new landing), types nothing, and restates the
/// landing in its journal.
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
    let (turns, told, notes, lines) = restart_on(
        "restart-land-awaited",
        std::slice::from_ref(&land_typed),
        &[],
        answered("land", LANDING_REPLY),
    );
    assert_eq!(
        turns, 1,
        "the land act's defense gets the check: {lines:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains(&format!("rule={RULE_LAND_CHECK} "))),
        "{lines:#?}"
    );
    assert_eq!(told, [TaskFace::Open]);
    assert!(notes.is_empty(), "{notes:#?}");
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
    let (turns, told, _, lines) = restart(
        "restart-moved-on",
        std::slice::from_ref(&land_typed),
        std::slice::from_ref(&note),
        PROGRESS,
    );
    assert_eq!(turns, 1, "{lines:#?}");
    assert_eq!(told, [TaskFace::Landed, TaskFace::Open]);
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

/// A RESTARTED LOOP LANDS ON ITS CHECK'S `LANDED`: the ledger's last Land act is the check, so
/// the answer the restarted loop reads first is judged as the check's, and lands with nothing
/// typed. Negative control: the journal's seed without `checked` (`LandSeed::checked`).
#[test]
fn a_restarted_loop_lands_on_its_checks_landed_answer() {
    let min = |n: u64| Duration::from_secs(60 * n);
    let ledger = [
        ledger_row(RULE_LAND, "land", min(10)),
        ledger_row(RULE_LAND_CHECK, "land check", min(2)),
    ];
    let (dir, path) = files("restart-check-awaited", &ledger, &[]);
    let host = Arc::new(Faces::default());
    let (lines, turns) = run(
        vec![answered("land check", LANDED)],
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
    assert_eq!(turns, 0, "the check's LANDED: {lines:#?}");
    assert_eq!(host.told(), [TaskFace::Open, TaskFace::Landed]);
    assert_eq!(notes.len(), 1, "{notes:#?}");
    assert!(
        notes[0].contains(&format!("rule={RULE_LAND} ")),
        "{notes:#?}"
    );
}

/// A REST ON A NEED SURVIVES A LOOP THAT FOLLOWS (the post-landing review, 2026-11-01): a loop
/// types Land's instruction, then the check at the defense, and rests on the check's answer, a
/// need alone, typing nothing; a loop that follows on the same ledger and the same screen reads
/// that point as the check's answer again and types nothing either. Negative control: the restarted
/// loop's seeded point re-judged as a defense (13a's `rejudged`, `land_defended` alone) types
/// `land-nudge@v1` into the agent that can only wait for the person.
#[test]
fn a_rest_on_a_need_survives_a_loop_that_follows() {
    let need = "NEEDS YOU: rotate the deploy key in the dashboard";
    let (dir, path) = files("rest-restarted", &[], &[]);
    let host = Arc::new(Faces::default());
    let screens = vec![
        busy_screen(),
        ended(PROGRESS),
        busy_screen(),
        ended(LANDING_REPLY),
        busy_screen(),
        ended(need),
    ];
    let (lines, turns) = run(screens, "land", vec![1, 3], &host, Some(&path), None);
    assert_eq!(turns, 2, "the instruction, then the check: {lines:#?}");
    assert!(
        lines
            .iter()
            .any(|l| l.contains(&format!("rule={RULE_LAND_CHECK} "))),
        "{lines:#?}"
    );
    let host = Arc::new(Faces::default());
    let (after, turns) = run(
        vec![ended(need)],
        "land",
        Vec::new(),
        &host,
        Some(&path),
        None,
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(turns, 0, "the rest stands: {after:#?}");
    assert!(
        !after.iter().any(|l| l.contains("rule=land-")),
        "nothing of Land's typed: {after:#?}"
    );
}

/// AN OFF NO PERSON SET KEEPS LAND'S REST ON A NEED, AT THE LOOP (the second review,
/// 2026-11-01): a loop rests on the check's answer, a need alone; `aterm ctl` then sets Off
/// (`autopilot_by=ctl`), and the park keeps the rest (its row `OFF_STOOD_RESTING`); the loop the
/// host starts as that Off lifts to Land, set by `aterm ctl` too, types nothing over the same
/// screen and keeps the need up. The person's own lift to Land is their turn: Land's act.
/// CONTROL: the person's own Off forgets the rest, and the lift, even by `aterm ctl`, types
/// Land's act. Negative control: `park_off_held` leaving the land episode (the first round's
/// park) writes `OFF_STOOD_HELD`, and the lift types `land@v1` into the rest.
#[test]
fn an_off_no_person_set_keeps_lands_rest_at_the_loop() {
    use crate::supervise::approvals::{OFF_STOOD, OFF_STOOD_RESTING};
    use crate::supervise::policy::land_check::WAITS_ON_YOU;
    let need = "NEEDS YOU: rotate the deploy key in the dashboard";
    let screens = || {
        vec![
            busy_screen(),
            ended(PROGRESS),
            busy_screen(),
            ended(LANDING_REPLY),
            busy_screen(),
            ended(need),
        ]
    };
    let rest_then_off = |tag: &str, by: &'static str| {
        // A run with no Off rests on the need and counts the `meta` reads its waits make.
        let (dry, dry_path) = files(&format!("{tag}-dry"), &[], &[]);
        let (lines, turns, m) = run_kept(
            screens(),
            "land",
            vec![1, 3],
            &Arc::new(Faces::default()),
            Some(&dry_path),
            None,
            |m| m.autopilot_by = Some(by),
        );
        let journal = journal_lines(&dry_path);
        let _ = std::fs::remove_dir_all(&dry);
        assert_eq!(
            turns, 2,
            "{tag}: the instruction, then the check: {lines:#?}"
        );
        assert!(
            journal
                .iter()
                .any(|l| l.starts_with("WAITING ") && l.contains(WAITS_ON_YOU)),
            "{tag}: the rest: {journal:#?}"
        );
        let metas = m.requests.iter().filter(|r| *r == "meta").count();
        // The same run, Off set at its next-to-last `meta` read, inside the rest's waits.
        let (dir, path) = files(tag, &[], &[]);
        let (lines, turns) = run_on(
            screens(),
            "land",
            vec![1, 3],
            &Arc::new(Faces::default()),
            Some(&path),
            None,
            |m| {
                m.autopilot_by = Some(by);
                m.autopilot_at = vec![(metas - 1, Some("off"))];
            },
        );
        assert_eq!(turns, 2, "{tag}: {lines:#?}");
        (dir, path)
    };
    let lands = |lines: &[String]| {
        lines
            .iter()
            .any(|l| l.starts_with("CONTINUED ") && l.contains(&format!("rule={RULE_LAND} ")))
    };
    for (by, reason) in [("ctl", OFF_STOOD_RESTING), ("human:chip", OFF_STOOD)] {
        let tag = format!("rest-off-{}", by.replace(':', "-"));
        for lift in ["ctl", "human:chip"] {
            let (dir, path) = rest_then_off(&format!("{tag}-{}", lift.replace(':', "-")), by);
            let body = std::fs::read_to_string(&path).expect("ledger");
            let off = body
                .lines()
                .rev()
                .find(|l| l.contains("\"rule_id\":\"autopilot-off@v1\""))
                .unwrap_or_else(|| panic!("{tag}: the Off's row: {body}"));
            let off: aterm_json::Value = aterm_json::from_str(off).expect("the Off's row");
            assert_eq!(
                off.get("reason").and_then(aterm_json::Value::as_str),
                Some(reason),
                "{tag}"
            );
            let (_, turns, added, after, m) = after_off_on(&path, "land", vec![ended(need)], |m| {
                m.autopilot_by = Some(lift);
            });
            let _ = std::fs::remove_dir_all(&dir);
            let types = by.starts_with("human:") || lift.starts_with("human:");
            assert_eq!(
                (turns > 0, lands(&after)),
                (types, types),
                "{tag}, lifted by {lift}: {after:#?}\n{added:#?}"
            );
            if !types {
                assert!(
                    !m.requests
                        .iter()
                        .any(|r| r.starts_with("meta unset attention owner=needs")),
                    "{tag}: the need stays up: {:#?}",
                    m.requests
                );
            }
        }
    }
}

/// THE NEEDS ENTRY SAYS THE WORK WAITS ON YOU WHERE NOTHING CARRIES ON (the second review,
/// 2026-11-01): where Land rests on the check's answer, a need alone, the menu bar's and the
/// border's entry ends `— waits on you`; a need the defense said stands beside the check's
/// `LANDED`, and the entry that said `— carrying on` while the check was out is told again with
/// the same need, ending `— waits on you`. Negative control: the tail kept at carrying on
/// (`Session::need_tail`) says the loop carries on beside a rest and a landing.
#[test]
fn the_needs_entry_says_the_work_waits_on_you_where_nothing_carries_on() {
    use crate::harness::hold_said::NeedTail;
    let need = "NEEDS YOU: rotate the deploy key in the dashboard";
    let ends = |tail: NeedTail| format!(" \u{2014} {}", tail.words());
    let entries = |m: &Mock| -> Vec<String> {
        m.requests
            .iter()
            .filter(|r| r.starts_with("meta set attention owner=needs "))
            .cloned()
            .collect()
    };
    let kept = |tag: &str, last: Vec<Vec<String>>| {
        let (dir, path) = files(tag, &[], &[]);
        let mut screens = vec![busy_screen(), ended(PROGRESS), busy_screen()];
        screens.extend(last);
        let (lines, turns, m) = run_kept(
            screens,
            "land",
            vec![1, 3],
            &Arc::new(Faces::default()),
            Some(&path),
            None,
            |_| {},
        );
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            turns, 2,
            "{tag}: the instruction, then the check: {lines:#?}"
        );
        entries(&m)
    };
    let told = kept(
        "needs-tail-rest",
        vec![ended(LANDING_REPLY), busy_screen(), ended(need)],
    );
    assert!(
        !told.is_empty()
            && told.iter().all(
                |e| e.contains("needs you: rotate") && e.ends_with(&ends(NeedTail::WaitsOnYou))
            ),
        "the rest: {told:#?}"
    );
    let told = kept(
        "needs-tail-landed",
        vec![
            ended(&format!("{LANDING_REPLY}\n{need}")),
            busy_screen(),
            ended(LANDED),
        ],
    );
    assert!(
        told.len() == 2
            && told[0].ends_with(&ends(NeedTail::CarryingOn))
            && told[1].ends_with(&ends(NeedTail::WaitsOnYou))
            && told[1].contains("needs you: rotate"),
        "the check out, then landed beside the need: {told:#?}"
    );
}

/// THE NEEDS ENTRY SAYS THE WORK WAITS ON YOU WHERE THE LOOP STOPPED TYPING (the post-landing
/// review, 2026-11-02): under Land, the agent gives the same blocked reply twice (its `NEEDS
/// YOU:` line kept although it worked), so the stop for replies that repeat holds the loop's
/// typing (`replies_stop`) and Land types nothing at the repeat; the entry is told again ending
/// ` — waits on you`, where it said ` — carrying on` while Land still typed. Under Cruise the
/// same repeat gets another turn past its back-off (`repeat_gets_a_turn`), so the entry keeps
/// ` — carrying on`.
/// Negative control: `need_tail` reading only Land's rest and landing (`origin/main`
/// `21b1e42025`) leaves the held entry ` — carrying on`.
#[test]
fn the_needs_entry_says_the_work_waits_on_you_where_the_loop_stopped_typing() {
    use crate::harness::hold_said::NeedTail;
    let blocked = "Blocked on the key.\nNEEDS YOU: add the deploy key in the dashboard";
    let ends = |tail: NeedTail| format!(" \u{2014} {}", tail.words());
    let told = |word: &'static str, gates: Vec<usize>, tag: &str| {
        let (dir, path) = files(tag, &[], &[]);
        let screens = vec![
            busy_screen(),
            ended(PROGRESS),
            busy_screen(),
            ended(blocked),
            busy_screen(),
            ended(blocked),
        ];
        let (lines, turns, m) = run_kept(
            screens,
            word,
            gates,
            &Arc::new(Faces::default()),
            Some(&path),
            None,
            |_| {},
        );
        let _ = std::fs::remove_dir_all(&dir);
        let entries: Vec<String> = m
            .requests
            .iter()
            .filter(|r| r.starts_with("meta set attention owner=needs "))
            .cloned()
            .collect();
        (lines, turns, entries)
    };
    let (lines, turns, entries) = told("land", vec![1, 3], "needs-tail-held");
    assert_eq!(
        turns, 2,
        "the instruction, then one act, none at the repeat: {lines:#?}"
    );
    assert!(
        entries
            .first()
            .is_some_and(|e| e.ends_with(&ends(NeedTail::CarryingOn))),
        "while Land still typed: {entries:#?}"
    );
    assert!(
        entries
            .last()
            .is_some_and(|e| e.contains("needs you: add the deploy key")
                && e.ends_with(&ends(NeedTail::WaitsOnYou))),
        "the held repeat: {entries:#?}\n{lines:#?}"
    );
    let (lines, _, entries) = told("cruise", vec![1, 3, 5], "needs-tail-cruise");
    assert!(
        !entries.is_empty()
            && entries
                .iter()
                .all(|e| e.ends_with(&ends(NeedTail::CarryingOn))),
        "Cruise carries on: {entries:#?}\n{lines:#?}"
    );
}

/// A `NEEDS YOU:` BESIDE A FAILED CONCLUSION WAITS ON THE PERSON: nothing carries the work on, so its
/// entry never says the harness does. NEGATIVE CONTROL: the same need beside a reply of work carries
/// on.
#[test]
fn a_need_beside_a_failed_conclusion_says_the_work_waits_on_you() {
    use crate::harness::hold_said::NeedTail;
    let need = "NEEDS YOU: rotate the deploy key in the dashboard";
    let told = |tag: &str, said: String| {
        let (dir, path) = files(tag, &[], &[]);
        let (lines, _, m) = run_kept_tracked(
            vec![busy_screen(), ended(&said)],
            "land",
            Vec::new(),
            &Arc::new(Faces::default()),
            Some(&path),
            None,
            Some(path.with_file_name("tasks")),
            |m| {
                m.task = Some((
                    1,
                    "state=working from=s-0a1b since_ms=0 result_bytes=0".to_string(),
                ));
            },
        );
        let _ = std::fs::remove_dir_all(&dir);
        let entries: Vec<String> = m
            .requests
            .iter()
            .filter(|r| r.starts_with("meta set attention owner=needs "))
            .cloned()
            .collect();
        assert!(!entries.is_empty(), "{tag}: {lines:#?}");
        entries
    };
    let ends = |tail: NeedTail| format!(" \u{2014} {}", tail.words());
    let gave_up = told("needs-tail-failed", format!("{COULD_NOT}\n{need}"));
    assert!(
        gave_up
            .iter()
            .all(|e| e.ends_with(&ends(NeedTail::WaitsOnYou))),
        "{gave_up:#?}"
    );
    let worked = told("needs-tail-worked", format!("{PROGRESS}\n{need}"));
    assert!(
        worked
            .iter()
            .any(|e| e.ends_with(&ends(NeedTail::CarryingOn))),
        "{worked:#?}"
    );
}

/// A DEFENSE THE CHECK AUDITED STAYS AUDITED ACROSS A LOOP THAT FOLLOWS (the post-landing
/// review, 2026-11-01): a loop types Land's instruction, the check at the defense, and the nudge
/// quoting the list the check's answer gave; the agent says the same defense again after a short
/// turn (no real work), and the loop judges that point, no second check owed, but ends before its
/// act is written (a crash between the two: its last nudge cut from the books). A loop that
/// follows on that ledger re-judges the point and types Land's nudge again, never the check, the
/// check's books ridden on the point's row. Negative control: the books not seeded (`run.rs`'s
/// `seed_land_books`) types the check a second time over that defense.
#[test]
fn a_defense_the_check_audited_is_not_checked_again_by_a_loop_that_follows() {
    let list = "The docs PR is still open.";
    let (dir, path) = files("books-restarted", &[], &[]);
    let host = Arc::new(Faces::default());
    let screens = vec![
        busy_screen(),
        ended(PROGRESS),
        busy_screen(),
        ended(LANDING_REPLY),
        busy_screen(),
        ended(list),
        busy_screen(),
        ended_after(LANDING_REPLY, "12s"),
    ];
    let (lines, _) = run(screens, "land", vec![1, 3, 5], &host, Some(&path), None);
    let typed = |lines: &[String], rule: &str| {
        lines
            .iter()
            .filter(|l| l.starts_with("CONTINUED ") && l.contains(&format!("rule={rule} ")))
            .count()
    };
    assert_eq!(
        (
            typed(&lines, RULE_LAND_CHECK),
            typed(&lines, RULE_LAND_NUDGE)
        ),
        (1, 2),
        "one check, then the nudge at the list and at the defense said again: {lines:#?}"
    );
    let cut_last = |file: &Path, needle: &str| {
        let body = std::fs::read_to_string(file).expect("books");
        let mut rows: Vec<&str> = body.lines().collect();
        let at = rows.iter().rposition(|l| l.contains(needle)).expect(needle);
        rows.remove(at);
        let body: String = rows.iter().map(|r| format!("{r}\n")).collect();
        std::fs::write(file, body).expect("books");
    };
    cut_last(&path, &format!("\"rule_id\":\"{RULE_LAND_NUDGE}\""));
    cut_last(
        &crate::supervise::approvals::journal_beside(&path),
        &format!("rule={RULE_LAND_NUDGE} "),
    );
    let host = Arc::new(Faces::default());
    let (after, _) = run(
        vec![ended_after(LANDING_REPLY, "12s")],
        "land",
        Vec::new(),
        &host,
        Some(&path),
        None,
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        (
            typed(&after, RULE_LAND_CHECK),
            typed(&after, RULE_LAND_NUDGE)
        ),
        (0, 1),
        "the defense stays audited: {after:#?}"
    );
}

/// A restart whose first point shows the landing's own words, no turn run since, keeps the
/// landing over a checkout that now holds work, without asking the probe
/// (`TurnEndState::landing_words_read`); a turn since, or a start after Off, asks it.
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

/// The reply that ends someone else's task after a landing, on the worker's token.
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

/// The first loop of a landing under Land: progress, the land instruction, its done reply, the
/// land check and its `LANDED`, then `then` (from screen 6), over the books at `ledger`, with a
/// person's hand on the screens `hand`.
fn land_first(
    ledger: &Path,
    then: Vec<Vec<String>>,
    hand: &[usize],
) -> (Vec<TaskFace>, usize, Vec<String>) {
    let host = Arc::new(Faces::default());
    let mut screens = vec![busy_screen(), ended(PROGRESS), busy_screen()];
    screens.extend(landing());
    screens.extend(then);
    let (lines, turns) = run_on(
        screens,
        "land",
        vec![1, 3],
        &host,
        Some(ledger),
        None,
        |m| {
            for &i in hand {
                m.human_on.insert(i, "0");
            }
        },
    );
    (host.told(), turns, lines)
}

/// A loop started as the session's Off lifts, under `word`, at the screen `said`, over the same
/// books: the faces told, the turns typed, the journal lines it added, and its lines.
fn after_off(
    ledger: &Path,
    word: &'static str,
    said: &str,
) -> (Vec<TaskFace>, usize, Vec<String>, Vec<String>) {
    let (told, turns, added, lines, _) = after_off_on(ledger, word, vec![ended(said)], |_| {});
    (told, turns, added, lines)
}

/// [`after_off`] over `screens`, the mock set up further by `more` and handed back.
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

/// [`THEIR_LANDING_REPLY`] rewrapped at another width (a resize's redraw): a new point to the
/// loop's key.
const THEIR_LANDING_REPLY_REWRAPPED: &str =
    "Added the module docstring, ran the tests\nand committed it.\nATERM-TASK-DONE";

/// A person's turn after a landing that ends on `ATERM-TASK-DONE` is never read as a landing by
/// the loop that follows Off, under Cruise or Land.
#[test]
fn someone_elses_done_turn_after_a_landing_is_never_read_as_the_landing() {
    for (tag, hand) in [
        ("together-their-turn", &[][..]),
        ("together-own-exchange", &[6, 7][..]),
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
            // A person's turn after the landing is no exchange of theirs under Land: the session
            // worked before it (owner ruling 2026-10-27).
            assert!(!told.contains(&TaskFace::Theirs), "{tag}: {log}");
        }
        assert_eq!(
            first.iter().filter(|l| landed_note(l)).count(),
            1,
            "{tag}: the landing, once: {log}"
        );
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

/// A turn ending on the worker's token after a landing, typed while Off stopped the loop, is no
/// landing.
#[test]
fn a_done_turn_no_loop_watched_after_a_landing_is_no_landing() {
    for word in ["cruise", "land"] {
        let (dir, ledger) = files(&format!("unwatched-{word}"), &[], &[]);
        let (told, turns, lines) = land_first(&ledger, Vec::new(), &[]);
        assert_eq!(turns, 2, "{lines:#?}");
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
        let (told, _, _, _) = after_off(&ledger, word, THEIR_LANDING_REPLY);
        assert_eq!(told.first(), Some(&TaskFace::Open), "{word}");
        assert!(!told.contains(&TaskFace::Landed), "{word}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A landing ridden through a seamless update's cut still reads landed after its restart. Every
/// request index a cut can fall on runs for one reply, each under the watcher's own checks (a
/// hang, a tape that does not replay). THE GAP: the other reply stops at the first cut that hands
/// the landing's point again, trying `SHORT_FIRST_CUT` first, so a cut at another index that
/// panics, hangs or breaks a tape's replay only under that reply is not seen.
#[test]
fn a_landing_ridden_through_an_update_cut_reads_landed_after_its_restart() {
    const SHORT_DEFENSE: &str =
        "Verified, committed and pushed the parser.\nNothing left: it is all on remote main.";
    /// The first cut that hands the short defense's landing again.
    const SHORT_FIRST_CUT: usize = 30;
    for (reply, first_cut) in [
        (SHORT_DEFENSE, Some(SHORT_FIRST_CUT)),
        (LANDING_REPLY, None),
    ] {
        let cut = |i: usize| {
            let (dir, ledger) = files("update-cut", &[], &[]);
            let host = Arc::new(Faces::default());
            let screens = vec![
                busy_screen(),
                ended(PROGRESS),
                busy_screen(),
                ended(reply),
                busy_screen(),
                ended(LANDED),
            ];
            let (_, turns) = run_on(
                screens,
                "land",
                vec![1, 3],
                &host,
                Some(&ledger),
                None,
                |m| {
                    m.by_index.insert(i, replaced(REPLACED_ANCHOR));
                    m.handoff_seq = Some(7);
                },
            );
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
            if again && turns == 2 && host.told() == [TaskFace::Open, TaskFace::Landed] {
                Some((dir, ledger, first))
            } else {
                let _ = std::fs::remove_dir_all(&dir);
                None
            }
        };
        let cut = &cut;
        let handed = if let Some(k) = first_cut {
            (k..56).chain(4..k).find_map(cut)
        } else {
            let mut handed: Vec<_> = std::thread::scope(|scope| {
                let tries: Vec<_> = (4..56usize).map(|i| scope.spawn(move || cut(i))).collect();
                tries
                    .into_iter()
                    .filter_map(|t| t.join().unwrap_or_else(|p| std::panic::resume_unwind(p)))
                    .collect()
            });
            for (later, _, _) in handed.iter().skip(1) {
                let _ = std::fs::remove_dir_all(later);
            }
            handed.truncate(1);
            handed.pop()
        };
        let (dir, ledger, first) =
            handed.unwrap_or_else(|| panic!("{reply:?}: no cut handed the landing's point again"));
        let (told, turns, added, lines) = after_off(&ledger, "land", LANDED);
        let log = format!("{first:#?}\n{added:#?}\n{lines:#?}");
        assert_eq!(turns, 0, "{reply:?}: nothing typed into landed work: {log}");
        assert_eq!(told, [TaskFace::Landed], "{reply:?}: {log}");
        assert!(added.iter().any(|l| landed_note(l)), "{reply:?}: {log}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A plain landing, its books the first loop's own, restarted as Off lifts: under Land it is
/// restated and nothing is typed; under Cruise the done check runs. The negative control of
/// `someone_elses_done_turn_after_a_landing_is_never_read_as_the_landing` and
/// `a_done_turn_no_loop_watched_after_a_landing_is_no_landing`.
#[test]
fn a_plain_landing_restarted_after_off_still_reads_landed() {
    let (dir, ledger) = files("plain-landing", &[], &[]);
    let (told, turns, lines) = land_first(&ledger, Vec::new(), &[]);
    let first = journal_lines(&ledger);
    let log = format!("{first:#?}\n{lines:#?}");
    assert_eq!(turns, 2, "the land instruction and its check: {log}");
    assert_eq!(told, [TaskFace::Open, TaskFace::Landed], "{log}");
    assert!(first.iter().any(|l| landed_note(l)), "{log}");
    let (told, turns, added, lines) = after_off(&ledger, "land", LANDED);
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

/// A host that takes the idle point after the `at`-th turn ran (the task's end) with a step that
/// moves the session (`done:resumed`), raising the loop's stop as it steps.
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

/// How the first loop of a task is cut at the task's end ([`ended_first_cut`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cut {
    /// Not cut: no host step comes, and the loop runs on to the session's end.
    No,
    /// The host's step takes the end's point and stops the loop there ([`EndTaker`]).
    HostStep,
    /// The reader of the loop's lines goes away as the end's point line is printed.
    ReaderGone,
    /// The host hangs the loop's connection up as the fold publishes the end (`task mark done …`,
    /// [`Mock::hang_up_at`]).
    HungUp,
}

/// The loop's stdout, whose reader goes away at its `ok + 1`-th `EVENT idle` line: that write
/// and every one after it fail (`BrokenPipe`).
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

/// The first loop of a task under Land, over the ledger and journal at `ledger`: the job's
/// progress, Land's instruction, then `last`, its answer, then the land check and its `LANDED`.
fn ended_first(ledger: &Path, last: &str, take: bool) -> (Vec<TaskFace>, usize, Vec<String>) {
    ended_first_cut(ledger, last, if take { Cut::HostStep } else { Cut::No })
}

/// [`ended_first`], the loop cut at the task's end as `cut` says.
fn ended_first_cut(ledger: &Path, last: &str, cut: Cut) -> (Vec<TaskFace>, usize, Vec<String>) {
    let word = "land";
    let screens = vec![
        busy_screen(),
        ended(PROGRESS),
        busy_screen(),
        ended(last),
        busy_screen(),
        ended(LANDED),
    ];
    let gates = vec![1, 3];
    let take = cut == Cut::HostStep;
    let stop = Arc::new(AtomicBool::new(false));
    let host = Arc::new(EndTaker::new(if take { 3 } else { usize::MAX }, &stop));
    let mut m = Mock::new(screens);
    m.history_served();
    m.autopilot = Some(word);
    m.turn_gates = gates;
    m.vanish_after = Some(6);
    m.cursor_on_caret = true;
    m.stall_sleep = Some(Duration::from_millis(20));
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
        let mut out = ReaderGone {
            ok: 2,
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

/// A loop over a landing's books alone, its screen the landing's reply: under Land it restates
/// the landing with the first note's time and types nothing; under Cruise the session goes on
/// (the done check) and its face is told `open`.
fn follow_landing(ledger: &Path, tag: &str, follower: &'static str, lifted: bool) {
    let at = note_time(ledger, RULE_LAND);
    let host = Arc::new(Faces {
        lifted,
        ..Faces::default()
    });
    let before = journal_lines(ledger).len();
    let (lines, turns) = run(
        vec![ended(LANDED)],
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

/// A landing the host's step took is journaled as its point, its `DONE …` note, then the step,
/// and a loop that follows reads it as the first loop did.
#[test]
fn a_tasks_end_the_hosts_step_took_is_noted_and_its_follower_types_nothing() {
    for take in [true, false] {
        for (follower, lifted) in [("land", false), ("land", true), ("cruise", true)] {
            let tag = format!("end-taken-{take}-{follower}-{lifted}");
            let (dir, ledger) = files(&tag, &[], &[]);
            let (told, turns, lines) = ended_first(&ledger, LANDING_REPLY, take);
            let first = journal_lines(&ledger);
            let log = format!("{tag}: {first:#?}\n{lines:#?}");
            assert_eq!(turns, 2, "the land instruction and its check: {log}");
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

/// A landing is noted however its loop is cut at its point (`Session::say_point`), and a loop that
/// follows reads it as the first loop did.
#[test]
fn a_tasks_end_is_noted_when_its_point_line_or_its_publish_fails() {
    for cut in [Cut::ReaderGone, Cut::HungUp] {
        for (follower, lifted) in [("land", false), ("land", true), ("cruise", true)] {
            let tag = format!("end-cut-{cut:?}-{follower}-{lifted}");
            let (dir, ledger) = files(&tag, &[], &[]);
            let (told, turns, lines) = ended_first_cut(&ledger, LANDING_REPLY, cut);
            let first = journal_lines(&ledger);
            let log = format!("{tag}: {first:#?}\n{lines:#?}");
            assert_eq!(turns, 2, "the land instruction and its check: {log}");
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

/// A person's turn after a landing whose reply claims no done seeds `open`, and stays open: under
/// Land it is no exchange of theirs, since the session worked before it (owner ruling
/// 2026-10-27), so Land carries it on with its nudge, nothing journals it LEFT and the face never
/// turns to theirs.
#[test]
fn a_persons_turn_after_a_landing_that_leaves_the_task_open_seeds_open() {
    let (dir, ledger) = files("open-own-exchange", &[], &[]);
    let (told, turns, lines) = land_first(&ledger, vec![busy_screen(), ended(PROGRESS)], &[6, 7]);
    assert_eq!(
        turns, 3,
        "the land instruction, its check, then its nudge: {lines:#?}"
    );
    assert!(
        lines.iter().any(
            |l| l.starts_with("CONTINUED ") && l.contains(&format!(" rule={RULE_LAND_NUDGE} "))
        ),
        "{lines:#?}"
    );
    assert_eq!(
        told,
        [TaskFace::Open, TaskFace::Landed, TaskFace::Open],
        "{lines:#?}"
    );
    let (told, _, added, lines) = after_off(&ledger, "land", PROGRESS);
    let log = format!("{added:#?}\n{lines:#?}");
    assert!(
        !told.contains(&TaskFace::Theirs),
        "open, never theirs: {log}"
    );
    assert!(!added.iter().any(|l| l.starts_with("LEFT ")), "{log}");
    assert!(!added.iter().any(|l| landed_note(l)), "{log}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A host owed one carry-on, as the window's host is after a relaunch: it wants the first idle
/// point, records the [`IdlePoint`] and every face, and answers as `relaunch::resume_as` does
/// (`done:said-done` at a task done under Land, else `continued`, typed).
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

/// A relaunched agent's first point under `word`, its screen `said`, its carry-on owed to `host`,
/// its ledger and journal at `ledger`: the loop's lines, the turns typed, and the journal's lines.
fn relaunched(
    said: &str,
    word: &'static str,
    host: &Arc<Carrier>,
    ledger: &Path,
) -> (Vec<String>, usize, Vec<String>) {
    relaunched_task(said, word, host, ledger, None)
}

/// [`relaunched`] of a session whose task record reads `task`.
fn relaunched_task(
    said: &str,
    word: &'static str,
    host: &Arc<Carrier>,
    ledger: &Path,
    task: Option<(u64, String)>,
) -> (Vec<String>, usize, Vec<String>) {
    let mut m = Mock::new(vec![ended(said)]);
    m.history_served();
    let tasks = task.is_some().then(|| ledger.with_file_name("tasks"));
    m.task = task;
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
        tasks,
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

/// A relaunched session whose landing is seeded is told done under Land at its first point: the
/// host types nothing (`carry-on step=done:said-done`), the landing is noted between the point's
/// line and the host's, and the face is landed; an unlanded relaunch gets its carry-on.
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
        ..IdlePoint::default()
    };
    let (points, faces, turns, notes, lines) = carried("carry-landed", LANDING_REPLY, "land", true);
    assert_eq!(points, [done], "{notes:#?}");
    assert_eq!(turns, 0, "nothing typed: {lines:#?}");
    assert_eq!(faces, [TaskFace::Landed], "{notes:#?}");
    let host_line = notes
        .iter()
        .position(|l| l.starts_with("HOST ") && l.ends_with("carry-on step=done:said-done"))
        .unwrap_or_else(|| panic!("the host's word: {notes:#?}"));
    let end = notes
        .iter()
        .rposition(|l| l.starts_with("DONE ") && l.contains(&format!(" rule={RULE_LAND} ")))
        .unwrap_or_else(|| panic!("the landing's note: {notes:#?}"));
    assert!(
        end > 0 && notes[end - 1].starts_with("EVENT idle ") && end < host_line,
        "the point, its landing, then the host's word: {notes:#?}"
    );
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

/// A FAILED CONCLUSION RESTS EXACTLY AS A LANDING DOES ACROSS A RELAUNCH: the journal's `FAILED` row
/// seeds it, the owed carry-on is settled at the first point with nothing typed, and the ending the
/// row names comes back. NEGATIVE CONTROLS: with no row a reply of work gets its carry-on; Cruise,
/// which never rests, carries on over the row; and a turn run since lifts it (the journal read gives
/// nothing).
#[test]
fn a_relaunched_task_that_failed_under_land_rests_and_typed_nothing() {
    use crate::supervise::policy::turn_end::GaveUp;
    let min = |n: u64| Duration::from_secs(60 * n);
    let carried = |tag: &str, said: &str, word: &'static str, seeded: bool| {
        let (dir, path) = if seeded {
            files(
                tag,
                &[ledger_row(RULE_LAND, "land", min(10))],
                &[journal_row(FAILED_NOTE, min(9), None)],
            )
        } else {
            files(tag, &[], &[])
        };
        let host = Arc::new(Carrier::default());
        let (lines, turns, notes) = relaunched(said, word, &host, &path);
        let _ = std::fs::remove_dir_all(&dir);
        (host.points(), turns, notes, lines)
    };
    let done = IdlePoint {
        done_under_land: true,
        ..IdlePoint::default()
    };
    for (tag, said, seeded) in [
        ("carry-failed", COULD_NOT, true),
        ("carry-failed-unread", PROGRESS, true),
    ] {
        let (points, turns, notes, lines) = carried(tag, said, "land", seeded);
        assert_eq!(points, [done], "{tag}: {notes:#?}");
        assert_eq!(turns, 0, "{tag}: nothing typed: {lines:#?}");
        assert!(
            notes
                .iter()
                .any(|l| l.starts_with("HOST ") && l.ends_with("carry-on step=done:said-done")),
            "{tag}: {notes:#?}"
        );
        assert!(
            notes.iter().any(|l| l.starts_with("FAILED ")),
            "{tag}: {notes:#?}"
        );
    }
    // No journal row: a worker's own reply concludes at the relaunch's first point.
    let (dir, path) = files("carry-failed-reply", &[], &[]);
    let host = Arc::new(Carrier::default());
    let task = Some((
        1,
        "state=working from=s-0a1b since_ms=0 result_bytes=0".to_string(),
    ));
    let (lines, turns, notes) = relaunched_task(COULD_NOT, "land", &host, &path, task);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        host.points().len() == 1 && host.points()[0].done_under_land,
        "{notes:#?}"
    );
    assert_eq!(turns, 0, "nothing typed: {lines:#?}");
    assert!(
        notes
            .iter()
            .any(|l| l.starts_with("HOST ") && l.ends_with("carry-on step=done:said-done")),
        "{notes:#?}"
    );
    for (tag, said, word, seeded) in [
        ("carry-failed-cold", PROGRESS, "land", false),
        // A session with no task record: the same words are a report like any other.
        ("carry-failed-no-task", COULD_NOT, "land", false),
        ("carry-failed-cruise", PROGRESS, "cruise", true),
    ] {
        let (points, _, notes, _) = carried(tag, said, word, seeded);
        assert_eq!(points, [IdlePoint::default()], "{tag}: {notes:#?}");
        assert!(
            notes
                .iter()
                .any(|l| l.starts_with("HOST ") && l.ends_with("carry-on step=continued")),
            "{tag}: {notes:#?}"
        );
    }

    let row = journal_row(FAILED_NOTE, min(9), None);
    assert_eq!(
        ended_before(&row, None, None).map(|(.., ending)| ending),
        Some(Some(GaveUp::CouldNotFinish))
    );
    assert_eq!(landed_before(&row, None, None), None, "no landing");
    assert_eq!(
        ended_before(&row, None, Some(i64::MAX)).map(|(.., ending)| ending),
        None,
        "a turn run since"
    );
    let unknown = journal_row(
        "FAILED seq=104 rule=land@v1 the worker fell over (autopilot land)",
        min(9),
        None,
    );
    assert_eq!(ended_before(&unknown, None, None).map(|_| ()), None);
}

/// The idle point a host is offered names what holds a fact it would type there of its own
/// (`IdlePoint::held`), read by the occupancy door's own guards at that point: a person who typed
/// within `human_grace_s`, a turn that was a person's own exchange, a manager waiting on the
/// session (its task record says it works). The exchange and the manager's wait hold the session
/// for as long as they keep it, so the point names them as theirs too (`IdlePoint::theirs`); a
/// person's grace alone does not. With none of them it names nothing, so the host types its
/// fact.
#[test]
fn an_idle_point_names_the_guard_that_holds_a_fact_the_host_would_type() {
    type More<'a> = &'a dyn Fn(&mut Mock);
    let held = |tag: &str, screens: Vec<Vec<String>>, grace: u32, managed: bool, more: More<'_>| {
        let host = Arc::new(Carrier::default());
        let (dir, ledger) = files(tag, &[], &[]);
        let mut m = Mock::new(screens);
        m.history_served();
        m.autopilot = Some("land");
        m.vanish_after = Some(30);
        m.cursor_on_caret = true;
        m.stall_sleep = Some(Duration::from_millis(20));
        if managed {
            m.task = Some((
                1,
                "state=working from=s-0a1b since_ms=0 result_bytes=0".to_string(),
            ));
        }
        more(&mut m);
        let opts = SuperviseOpts {
            policy: SupervisorConfig {
                human_grace_s: grace,
                ..owners_policy()
            },
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            tasks: managed.then(|| dir.join("tasks")),
            ..auto(30)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_turn_end_timing(TurnEndTiming {
                short_backoff: Duration::from_millis(30),
                short_backoff_max: Duration::from_millis(60),
                ..TurnEndTiming::default()
            });
            s.set_approval_ledger(Some(ledger.clone()));
        });
        let _ = std::fs::remove_dir_all(&dir);
        let points = host.points();
        (
            points.first().map(|p| (p.held, p.theirs)),
            format!("{points:?}\n{lines:#?}"),
        )
    };
    let none = |_: &mut Mock| {};
    let (word, log) = held("held-person", vec![ended(PROGRESS)], 60, false, &|m| {
        m.human_ms = Some(500);
    });
    assert_eq!(
        word,
        Some((Some(Guard::Person), None)),
        "a person typed half a second ago: {log}"
    );
    let screens = vec![busy_screen(), ended(PROGRESS)];
    let (word, log) = held("held-exchange", screens, 0, false, &|m| {
        m.human_on.insert(0, "0");
        m.human_on.insert(1, "0");
    });
    assert_eq!(
        word,
        Some((Some(Guard::Exchange), Some(Guard::Exchange))),
        "the turn was the person's own: {log}"
    );
    let (word, log) = held("held-managed", vec![ended(PROGRESS)], 0, true, &none);
    assert_eq!(
        word,
        Some((Some(Guard::Managed), Some(Guard::Managed))),
        "a manager waits on the session: {log}"
    );
    let (word, log) = held(
        "held-managed-person",
        vec![ended(PROGRESS)],
        60,
        true,
        &|m| {
            m.human_ms = Some(500);
        },
    );
    assert_eq!(
        word,
        Some((Some(Guard::Person), Some(Guard::Managed))),
        "a person at a managed worker: the person holds it now, the manager for good: {log}"
    );
    let (word, log) = held("held-none", vec![ended(PROGRESS)], 60, false, &none);
    assert_eq!(word, Some((None, None)), "nothing holds it: {log}");
}

/// A host that restarts its agent for the memory banner (`IdleHost::restart`: `adopted`) and
/// then owes the relaunch's carry-on at the next idle point, answered as [`Carrier`] does.
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
    fn restart(
        &self,
        why: &crate::supervise::policy::turn_end::Restart,
        _: &crate::supervise::policy::turn_end::RestartAsk,
    ) -> Option<crate::supervise::policy::turn_end::RestartAnswer> {
        self.restarts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(why.word().to_string());
        self.carry.store(true, Ordering::SeqCst);
        Some(crate::supervise::policy::turn_end::RestartAnswer::unread(
            "adopted",
        ))
    }
}

/// A land instruction whose turn ends at the memory banner is carried on (`continued`) after the
/// restart: the job's done report before it is no landing. A landing made before the banner is
/// told done under Land and types nothing.
#[test]
fn a_landing_the_memory_banner_cut_is_carried_on_after_its_restart() {
    use aterm_phase::prompt::fixtures::{MEMORY_BANNER_IDLE, screen};
    let done = "All done. Nothing left to do.";
    let restarted = |tag: &str, landing: Vec<Vec<String>>, after: &str| {
        let gates = if landing.is_empty() {
            vec![1]
        } else {
            vec![1, 3]
        };
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
        m.turn_gates = gates;
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
    let (points, restarts, notes, lines) =
        restarted("carry-landed-then-banner", landing().to_vec(), LANDED);
    let log = format!("{notes:#?}\n{lines:#?}");
    assert!(
        restarts.iter().any(|w| w == "memory"),
        "{restarts:?}: {log}"
    );
    assert_eq!(
        points.first(),
        Some(&IdlePoint {
            done_under_land: true,
            ..IdlePoint::default()
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

/// A loop started under Cruise over a landing's books, fresh or as Off lifts, tells no landing;
/// one under Land restates it.
#[test]
fn a_cruise_loop_over_a_landings_books_tells_no_landing() {
    let (dir, ledger) = files("cruise-over-landing", &[], &[]);
    let (told, turns, lines) = ended_first(&ledger, LANDING_REPLY, false);
    assert_eq!(turns, 2, "{lines:#?}");
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
    let (dir, ledger) = files("land-over-landing", &[], &[]);
    let (told, _, lines) = ended_first(&ledger, LANDING_REPLY, false);
    assert_eq!(told, [TaskFace::Open, TaskFace::Landed], "{lines:#?}");
    let at_land = note_time(&ledger, RULE_LAND);
    let host = Arc::new(Faces::default());
    let (lines, turns) = run(
        vec![ended(LANDED)],
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

/// A loop whose first read is the landed task's own wake keeps no landing over work the probe
/// then finds (`TurnEndState::wake_start_owed`): no probe of its own saw the wake begin, so the
/// work may be the wake's, or a person's edit made under Off.
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
        let (turns, told, lines) = go(&format!("attach-clean-{lifted}"), lifted, None);
        assert_eq!(turns, 0, "lifted={lifted}: {lines:#?}");
        assert_eq!(
            told.last(),
            Some(&TaskFace::Landed),
            "lifted={lifted}: {told:?}"
        );
    }
}

/// A landed task's wake whose `NEEDS YOU:` is above the screen keeps no landing: the turn is read
/// whole (`TurnEndState::reads_turn_whole`).
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
    let (tall, turns, told, raised, lines) = go(20, false);
    assert!(tall);
    assert!(!raised, "{lines:#?}");
    assert_eq!(turns, 0, "{lines:#?}");
    assert_eq!(told.last(), Some(&TaskFace::Landed), "{told:?}");
}

/// A restart after the landed task's own wake, kept landed, keeps the landing
/// (`TurnEndState::take_wake_restated`, `TurnEndState::seed_landed_by_wake`).
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
        let mut screens = vec![busy_screen(), ended(PROGRESS), busy_screen()];
        screens.extend(landing());
        if with_wake {
            screens.extend([busy_screen(), ended(wake)]);
        }
        let (lines, turns) = run(screens, "land", vec![1, 3], &host, Some(&ledger), None);
        assert_eq!(turns, 2, "{lines:#?}");
        assert_eq!(
            host.told(),
            [TaskFace::Open, TaskFace::Landed],
            "{lines:#?}"
        );
        let last = if with_wake { wake } else { LANDED };
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
        let (_, told, lines) = restart(&ledger, "Rebased the docs branch onto main.");
        assert_ne!(told.last(), Some(&TaskFace::Landed), "{lines:#?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A switch from Land to Cruise on the landed screen is read at the wake's end
/// (`Session::turn_end_at_point`'s fresh word), so Cruise goes on instead of keeping the landing.
#[test]
fn a_switch_to_cruise_on_the_landed_screen_is_read_at_the_wakes_end() {
    let wake = "The monitor fired: the deploy finished.";
    let go = |switch: bool| {
        let (dir, ledger) = files(&format!("cruise-at-wake-{switch}"), &[], &[]);
        let host = Arc::new(Faces {
            finished: Some(true),
            ..Faces::default()
        });
        let mut screens = vec![busy_screen(), ended(PROGRESS), busy_screen()];
        screens.extend(landing());
        screens.extend([busy_screen(), ended(wake), ended(wake)]);
        let (lines, turns) = run_on(
            screens,
            "land",
            vec![1, 3],
            &host,
            Some(&ledger),
            None,
            |m| {
                if switch {
                    m.autopilot_on.insert(6, Some("cruise"));
                }
                m.vanish_after = Some(12);
            },
        );
        let _ = std::fs::remove_dir_all(&dir);
        let typed: Vec<String> = lines
            .iter()
            .filter(|l| l.starts_with("CONTINUED "))
            .cloned()
            .collect();
        (turns, host.told(), typed, lines)
    };
    let (turns, told, typed, lines) = go(true);
    assert_eq!(turns, 3, "{lines:#?}");
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
    let (turns, told, typed, lines) = go(false);
    assert_eq!(turns, 2, "{lines:#?}");
    assert_eq!(told, [TaskFace::Open, TaskFace::Landed], "{lines:#?}");
    assert_eq!(typed.len(), 2, "{typed:#?}");
}

/// What a client says where nothing serves the control socket yet, as a seamless update's
/// successor's first bare `meta` finds before its socket listens.
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

/// A landing restarted under the session's own Land by a loop whose start-up `meta` is unserved
/// still reads landed.
#[test]
fn a_landing_whose_successors_first_meta_is_unserved_still_reads_landed() {
    for lose in [false, true] {
        let (dir, ledger) = files(&format!("unserved-meta-{lose}"), &[], &[]);
        let (told, turns, _) = land_first(&ledger, Vec::new(), &[]);
        assert_eq!(turns, 2, "lose={lose}: the land instruction and its check");
        assert_eq!(told, [TaskFace::Open, TaskFace::Landed], "lose={lose}");
        let (told, turns, added, lines, m) =
            after_off_on(&ledger, "land", vec![ended(LANDED)], |m| {
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

/// A session's own Land under a Cruise table, its start-up `meta` unserved, gets Land's
/// instruction at its first point: the word is read where the decision acts
/// (`Session::autopilot_read`), not left to the table.
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

/// A restart over a landing's books under the session's `word` and the table's `table`, its first
/// bare `meta` lost where `lose`, the host started as `lifted` says, the mock set up further by
/// `more`: the faces told, the loop's lines, the turns typed, whether each bare `meta` was served,
/// and the journal lines it added.
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

/// An Off session's landing survives a lost start-up `meta`, and is restated when the owner turns
/// it to Land.
#[test]
fn an_off_landing_whose_first_meta_is_lost_reads_as_if_it_were_served() {
    for lose in [false, true] {
        let (dir, ledger) = files(&format!("off-land-lost-{lose}"), &[], &[]);
        let (told, turns, _) = land_first(&ledger, Vec::new(), &[]);
        assert_eq!(
            (told.as_slice(), turns),
            ([TaskFace::Open, TaskFace::Landed].as_slice(), 2)
        );
        let (told, lines, turns, metas, added) = restart_over_landing(
            &ledger,
            vec![ended(LANDED)],
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

/// A restart over a landing's books whose start-up `meta` is lost tells no face before the word
/// is read, then the one a served start tells.
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
                    ([TaskFace::Open, TaskFace::Landed].as_slice(), 2)
                );
                let (told, lines, _, metas, _) = restart_over_landing(
                    &ledger,
                    vec![ended(LANDED)],
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
