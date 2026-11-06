// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A worker's task is its launch's own prompt, so the conversation's record a host reads for
//! whether anyone asked it anything ([`IdleHost::taskless`], Claude Code's transcript) can only
//! lag that task at the loop's first point: a session whose task record stands is not rested on
//! the record's word there, even before the record holds the prompt. Every later point, and every
//! point of a session with no standing record, goes by the record's word, and the screen's own
//! taskless reads hold a managed worker as they hold any session. Where that word alone rests a
//! point whose screen shows a conversation, the point is decided again once, soon, so a record
//! that trails a reply is read again.

use super::*;

/// A host whose conversation record holds no prompt (`taskless`): what Claude Code 2.1.318's
/// transcript held 0.4 s after a worker's launch on the lab's stub, its mode rows and
/// file-history snapshots written and the task's row not, though the stub's instant reply had
/// already ended the turn on screen. [`ConversationRecord::new`]`(false)` is a record that holds
/// the prompt.
#[derive(Debug)]
struct ConversationRecord {
    /// How many of the host's asks the record answers before it holds the prompt.
    taskless_asks: usize,
    asked: std::sync::atomic::AtomicUsize,
}

impl ConversationRecord {
    fn new(taskless: bool) -> Self {
        Self::trailing(if taskless { usize::MAX } else { 0 })
    }

    /// A record that answers its first `asks` asks with no prompt and holds the prompt from then
    /// on: one Claude Code writes the task's row into after the reply ended the turn on screen.
    fn trailing(asks: usize) -> Self {
        Self {
            taskless_asks: asks,
            asked: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

impl IdleHost for ConversationRecord {
    fn wants(&self) -> bool {
        false
    }
    fn at_idle(&self) -> Option<HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn taskless(&self) -> bool {
        self.asked.fetch_add(1, Ordering::SeqCst) < self.taskless_asks
    }
}

/// What a loop's run left: its journal's lines, the host's `taskless` asks and the
/// session's `task` reads.
struct Ran {
    lines: Vec<String>,
    asked: usize,
    reads: usize,
}

impl Ran {
    /// The journal's `CONTINUED` lines.
    fn continued(&self) -> Vec<&String> {
        self.lines
            .iter()
            .filter(|l| l.starts_with("CONTINUED "))
            .collect()
    }
}

/// A loop of a minute over `screens` under Land (unless `set` gives the mock another Autopilot
/// word), the session's task record `task`, under a host whose conversation record is `record`;
/// `set` shapes the mock further.
fn supervised(
    tag: &str,
    screens: Vec<Vec<String>>,
    task: Option<(u64, String)>,
    record: ConversationRecord,
    set: impl FnOnce(&mut Mock),
) -> Ran {
    supervised_for(60, tag, screens, task, record, set)
}

/// [`supervised`], the loop running `max_s` seconds at most.
fn supervised_for(
    max_s: u64,
    tag: &str,
    screens: Vec<Vec<String>>,
    task: Option<(u64, String)>,
    record: ConversationRecord,
    set: impl FnOnce(&mut Mock),
) -> Ran {
    let (dir, journal) = journal_file(tag);
    let tasks = dir.join("tasks");
    let host = Arc::new(record);
    let mut m = Mock::new(screens);
    m.autopilot = Some("land");
    m.task = task;
    m.vanish_after = Some(3);
    set(&mut m);
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        journal: Some(journal.clone()),
        tasks: Some(tasks),
        ..hosted(max_s)
    };
    let _ = watch_lines(&mut m, &opts);
    let lines = journal_records(&journal)
        .0
        .into_iter()
        .map(|r| r.line)
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    Ran {
        lines,
        asked: host.asked.load(Ordering::SeqCst),
        reads: m.task_requests.iter().filter(|r| *r == "task").count(),
    }
}

/// [`supervised`] over one instant reply under a record that holds no prompt.
fn first_point(tag: &str, task: Option<(u64, String)>, set: impl FnOnce(&mut Mock)) -> Ran {
    let screens = vec![ended_quick(LAB_REPLY)];
    supervised(tag, screens, task, ConversationRecord::new(true), set)
}

/// A task record whose agent exited (`state=exited`): no manager waits on the session.
fn exited_task() -> Option<(u64, String)> {
    Some((
        2,
        "state=exited from=s-0a1b since_ms=0 result_bytes=0 exit=0".to_string(),
    ))
}

/// A MANAGED WORKER WHOSE FIRST REPLY CAME AT ONCE IS CONTINUED AT THAT POINT, once: its task
/// record stands (`state=working`, its manager waits), so the host's record, which holds no prompt
/// yet, is not asked whether the conversation was asked anything, and Land types its instruction
/// at the first point. NEGATIVE CONTROLS: with no task record, and with a record whose agent
/// exited, the same record's word rests the point as nobody's (`NeverTypeIntoATasklessSession`).
#[test]
fn a_managed_workers_first_point_is_continued_before_its_record_holds_the_task() {
    let ran = first_point("managed-first-point", working_task(), |_| {});
    let continued = ran.continued();
    assert_eq!(continued.len(), 1, "continued once: {:#?}", ran.lines);
    assert!(
        continued[0].starts_with(&format!("CONTINUED seq=101 rule={RULE_LAND} ")),
        "the managed worker's first point is continued: {:#?}",
        ran.lines
    );
    assert_eq!(ran.asked, 0, "the record is not asked of a managed worker");
    for (tag, task) in [
        ("unmanaged-first-point", None),
        ("exited-first-point", exited_task()),
    ] {
        let ran = first_point(tag, task, |_| {});
        assert!(
            ran.lines
                .iter()
                .any(|l| l == "HOLD seq=101 point nothing why=policy"),
            "the control ({tag}): the session rests on its record's word: {:#?}",
            ran.lines
        );
        assert!(
            ran.continued().is_empty(),
            "the control ({tag}): nothing is typed into it: {:#?}",
            ran.lines
        );
        assert!(ran.asked > 0, "the control ({tag}): the record is asked");
    }
}

/// A FIRST POINT JUDGED WITHOUT ITS TASK RECORD IS JUDGED AGAIN UNTIL THE RECORD READS: the
/// session's `task` read is turned away at the first point and again when the point is decided
/// again (`ERR control server busy`), so the point is judged as a session with no record is, on
/// the host's record's word, and rests each time; with the screen at rest no next point comes, so
/// the point is decided again soon each time, the record read then stands, the host's record is
/// asked no more, and Land types its instruction there, once. NEGATIVE CONTROL: a session with no
/// record, its reads turned away the same way, is read until served and still rests.
#[test]
fn a_managed_workers_first_point_judged_without_its_record_is_judged_again_once_it_reads() {
    let lost = |m: &mut Mock| {
        m.task_lost = 2;
        m.stall_sleep = Some(Duration::from_secs(20));
        m.vanish_after = Some(8);
    };
    let ran = first_point("managed-record-lost", working_task(), lost);
    assert!(
        ran.lines
            .iter()
            .any(|l| l == "HOLD seq=101 point nothing why=policy"),
        "judged first without the record: {:#?}",
        ran.lines
    );
    let continued = ran.continued();
    assert_eq!(
        continued.len(),
        1,
        "continued once the record read: {:#?}",
        ran.lines
    );
    assert!(
        continued[0].starts_with("CONTINUED seq=")
            && continued[0].contains(&format!(" rule={RULE_LAND} ")),
        "{:#?}",
        ran.lines
    );
    assert_eq!(ran.reads, 3, "turned away twice, then served");
    assert_eq!(
        ran.asked, 2,
        "the host's record is asked at the two decisions without the task record, and not at the \
         one with it"
    );
    let ran = first_point("unmanaged-record-lost", None, lost);
    assert_eq!(ran.reads, 3, "the control: turned away twice, then served");
    assert!(
        ran.continued().is_empty(),
        "the control: nothing is typed into it: {:#?}",
        ran.lines
    );
}

/// A MANAGED WORKER'S LATER POINT GOES BY THE HOST'S RECORD: only the launch's first reply can
/// come before the record holds the task, so once Land has typed at the first point, a later point
/// whose record holds no prompt (a conversation `/clear` began, a fresh relaunch) rests as any
/// session's does, and one whose record holds the prompt only once that point is decided again is
/// continued then. NEGATIVE CONTROL: a record that holds the prompt is continued at both points.
#[test]
fn a_managed_workers_later_point_rests_on_its_records_word() {
    let screens = || {
        vec![
            ended_quick(LAB_REPLY),
            busy_screen(),
            ended("Wrote the parser's error paths and their tests."),
        ]
    };
    let ran = supervised(
        "managed-later-point",
        screens(),
        working_task(),
        ConversationRecord::new(true),
        |_| {},
    );
    let continued = ran.continued();
    assert_eq!(continued.len(), 1, "{:#?}", ran.lines);
    assert!(
        continued[0].starts_with("CONTINUED seq=101 "),
        "{:#?}",
        ran.lines
    );
    assert!(
        ran.lines
            .iter()
            .any(|l| l.starts_with("HOLD seq=") && l.ends_with(" point nothing why=policy")),
        "the later point rests: {:#?}",
        ran.lines
    );
    assert!(ran.asked > 0, "the later point asks the record");
    let ran = supervised(
        "managed-later-point-tasked",
        screens(),
        working_task(),
        ConversationRecord::new(false),
        |_| {},
    );
    assert_eq!(ran.continued().len(), 2, "the control: {:#?}", ran.lines);
    let ran = supervised(
        "managed-later-point-trails",
        screens(),
        working_task(),
        ConversationRecord::trailing(1),
        |m| {
            m.stall_sleep = Some(Duration::from_secs(20));
            m.vanish_after = Some(6);
        },
    );
    let continued = ran.continued();
    assert_eq!(
        continued.len(),
        2,
        "a later point whose record trails its reply is continued once the record holds the \
         prompt: {:#?}",
        ran.lines
    );
    assert!(
        !continued[1].starts_with("CONTINUED seq=101 "),
        "{:#?}",
        ran.lines
    );
}

/// Claude Code's launch card over an empty composer: a conversation nobody has asked anything
/// ([`aterm_phase::turn::fresh`]).
fn launch_card() -> Vec<String> {
    let mut r = rows(&[
        " ▐▛███▛█   Claude Code v2.1.318",
        "▝▜██████▀  Opus 5.5 · Claude Max",
        " ▝▝   ▝▝   /w",
        "",
    ]);
    r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
    r
}

/// THE SCREEN'S OWN TASKLESS READ HOLDS A MANAGED WORKER: its launch card over an empty composer
/// is no reply to continue, whatever its records say. NEGATIVE CONTROL: the same session at a
/// reply is continued.
#[test]
fn a_managed_workers_taskless_screen_rests_whatever_its_record() {
    let ran = supervised(
        "managed-launch-card",
        vec![launch_card()],
        working_task(),
        ConversationRecord::new(false),
        |_| {},
    );
    assert!(ran.continued().is_empty(), "{:#?}", ran.lines);
    let ran = supervised(
        "managed-launch-card-control",
        vec![ended_quick(LAB_REPLY)],
        working_task(),
        ConversationRecord::new(false),
        |_| {},
    );
    assert_eq!(ran.continued().len(), 1, "the control: {:#?}", ran.lines);
}

/// A FIRST POINT THE HOST'S RECORD ALONE RESTS WHERE ITS SCREEN SHOWS A REPLY IS DECIDED AGAIN,
/// ONCE: a session with no task record under Cruise, whose reply came at once and ended its first
/// turn before Claude Code wrote the prompt's row, is read as a conversation nobody asked only by
/// that record; the point rests, is decided again soon, the record then holds the prompt, and
/// Cruise continues the session there (past its back-off for a short turn), once. NEGATIVE CONTROL: a record that never holds a prompt
/// rests the point at both looks and is looked at no more.
#[test]
fn a_point_its_hosts_record_alone_rests_under_a_reply_is_decided_again_once() {
    let cruise = |m: &mut Mock| {
        m.autopilot = Some("cruise");
        m.stall_sleep = Some(Duration::from_secs(20));
        m.vanish_after = Some(12);
    };
    let screens = || vec![ended_quick(LAB_REPLY)];
    let ran = supervised_for(
        300,
        "record-trails-reply",
        screens(),
        None,
        ConversationRecord::trailing(1),
        cruise,
    );
    assert!(
        ran.lines
            .iter()
            .any(|l| l == "HOLD seq=101 point nothing why=policy"),
        "rested first on the record's word: {:#?}",
        ran.lines
    );
    let continued = ran.continued();
    assert_eq!(
        continued.len(),
        1,
        "continued once the record holds the prompt: {:#?}",
        ran.lines
    );
    assert!(
        continued[0].starts_with("CONTINUED seq="),
        "{:#?}",
        ran.lines
    );
    let ran = supervised_for(
        300,
        "record-never-holds",
        screens(),
        None,
        ConversationRecord::new(true),
        cruise,
    );
    assert!(
        ran.continued().is_empty(),
        "the control: nothing is typed into it: {:#?}",
        ran.lines
    );
    assert_eq!(
        ran.asked, 2,
        "the control: asked at both looks, and no more"
    );
}
