// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for task record grammar, idempotent marks, handoff rows, and `await task` parsing.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::*;

const SID: &str = "s-0123456789abcdef0123";

#[test]
fn a_replacement_agent_keeps_its_task_open_across_a_late_exit() {
    use crate::session_timeline::{AgentStamp, SessionTimeline};
    use aterm_phase::Program;

    let mut timeline = SessionTimeline::default();
    let now = Instant::now();
    let asks = Asks::default();
    timeline.mark_task(&TaskMark::new(TaskState::Working), &asks, now, 1);
    timeline.set_task_worker(Program::Claude, None);
    let mut exited = TaskMark::new(TaskState::Exited);
    exited.why = Some(WHY_EXIT);
    timeline.mark_task(&exited, &asks, now, 2);
    assert_eq!(timeline.task().unwrap().state, TaskState::Exited);
    assert!(timeline.task().unwrap().has_news());
    let carried = timeline.task().unwrap().carry_row(now);
    timeline.seed_task(TaskRecord::from_carry_row(&carried, None, now, 2).unwrap());
    assert!(
        timeline.task().unwrap().unfinished_exit,
        "an update must preserve that this exit interrupted unfinished work"
    );

    // The shell/wrapper ended before the new Claude process was identified.
    timeline.note_foreground_group(42);
    timeline.set_program(42, Some("claude".to_string()));
    timeline.publish_agent(
        "busy",
        None,
        None,
        Some(Program::Claude),
        AgentStamp {
            generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
            fp: 1,
        },
    );
    let working = timeline.task().unwrap().clone();
    // Negative control: the old unconditional ledger write closes this very same task.
    let (unfenced, changed) = exited.apply(Some(&working), &asks, now, 3);
    assert!(changed);
    assert_eq!(unfenced.state, TaskState::Exited);
    assert!(unfenced.has_news());
    assert_eq!(working.state, TaskState::Working);
    assert_eq!(working.why, Some("moved"));
    assert!(!working.has_news());
    assert_eq!(working.started_unix_ms, 1);
    assert_eq!(working.agent, Some(Program::Claude));

    // A delayed exit of the old process must not close the replacement's task.
    timeline.mark_task(&exited, &asks, now, 3);
    assert_eq!(timeline.task().unwrap().rev, working.rev);
    assert_eq!(timeline.task().unwrap().state, TaskState::Working);

    // A real exit still concludes the task once the foreground agent is gone.
    timeline.note_foreground_group(7);
    timeline.mark_task(&exited, &asks, now, 4);
    assert_eq!(
        timeline.task().unwrap().state,
        TaskState::Working,
        "a foreground change can precede the sweep clearing its busy word"
    );
    timeline.publish_agent(
        "-",
        None,
        None,
        None,
        AgentStamp {
            generation: crate::control::ScreenGen { epoch: 1, seq: 2 },
            fp: 2,
        },
    );
    assert_eq!(
        timeline.task().unwrap().state,
        TaskState::Exited,
        "an exit arriving before the publication cleared is not lost"
    );
    timeline.mark_task(&exited, &asks, now, 4);
    assert_eq!(timeline.task().unwrap().state, TaskState::Exited);
}

#[test]
fn an_agent_return_reopens_only_an_unfinished_agent_exit() {
    use crate::session_timeline::{AgentStamp, SessionTimeline};
    use aterm_phase::Program;

    for (state, why, program_worker, expected) in [
        (TaskState::Exited, Some(WHY_EXIT), false, TaskState::Working),
        (TaskState::Done, Some(WHY_LANDED), false, TaskState::Done),
        (
            TaskState::Failed,
            Some(WHY_COULD_NOT_FINISH),
            false,
            TaskState::Failed,
        ),
        (TaskState::Exited, Some(WHY_EXIT), true, TaskState::Exited),
    ] {
        let mut timeline = SessionTimeline::default();
        let now = Instant::now();
        if expected == TaskState::Working {
            timeline.mark_task(&TaskMark::new(TaskState::Working), &Asks::default(), now, 0);
        }
        let mut mark = TaskMark::new(state);
        mark.why = why;
        timeline.mark_task(&mark, &Asks::default(), now, 1);
        if program_worker {
            timeline.set_task_worker(Program::Generic, None);
            timeline.set_task_program(Some("bench"));
        }
        // An editor is not evidence that an agent came back.
        timeline.note_foreground_group(42);
        timeline.set_program(42, Some("vim".to_string()));
        assert_eq!(timeline.task().unwrap().state, state);
        // A screen-identified agent also works before its process name is resolved.
        timeline.note_foreground_group(43);
        timeline.publish_agent(
            "busy",
            None,
            None,
            Some(Program::Claude),
            AgentStamp {
                generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
                fp: 1,
            },
        );
        assert_eq!(
            timeline.task().unwrap().state,
            expected,
            "{state:?}, {program_worker}"
        );
        if program_worker {
            timeline.mark_task(&TaskMark::new(TaskState::Working), &Asks::default(), now, 2);
            timeline.mark_task(&mark, &Asks::default(), now, 3);
            assert_eq!(
                timeline.task().unwrap().state,
                TaskState::Exited,
                "a program worker's exit is not deferred by an agent screen"
            );
        }
    }
}

#[test]
fn task_rows_keep_the_exit_and_agent_from_the_same_observation() {
    use crate::session_timeline::AgentStamp;
    use aterm_phase::Program;

    let handle = crate::session_store::test_handle(16);
    let ctx = &handle.ctx;
    mark_session_task(ctx, 16, &TaskMark::new(TaskState::Working)).expect("unfenced");
    let mut exit = TaskMark::new(TaskState::Exited);
    exit.why = Some(WHY_EXIT);
    mark_session_task(ctx, 16, &exit).expect("unfenced");
    let before = task_reading(ctx);
    {
        let mut timeline = ctx.timeline.lock().unwrap();
        timeline.note_foreground_group(42);
        timeline.set_program(42, Some("claude".to_string()));
        timeline.publish_agent(
            "busy",
            None,
            None,
            Some(Program::Claude),
            AgentStamp {
                generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
                fp: 1,
            },
        );
    }
    let after = task_reading(ctx);
    assert_eq!(
        (before.record.as_ref().unwrap().state, before.agent),
        (TaskState::Exited, "-")
    );
    assert_eq!(
        (after.record.as_ref().unwrap().state, after.agent),
        (TaskState::Working, "busy")
    );
    // Negative control: the old split read combines the prior record with the new verdict.
    let split = await_row(
        SID,
        before.record.as_ref(),
        after.agent,
        &after.program,
        &after.attention,
        false,
    );
    assert!(split.contains("state=exited agent=busy"));
    for read in [&before, &after] {
        let row = await_row(
            SID,
            read.record.as_ref(),
            read.agent,
            &read.program,
            &read.attention,
            false,
        );
        assert!(!row.contains("state=exited agent=busy"), "{row}");
    }
    let reply = cmd_task(ctx, 16, Scope::Owner, "");
    assert!(reply.contains("state=working"), "{reply}");
    assert!(reply.contains("agent=busy"), "{reply}");
    assert_eq!(
        cmd_result(ctx, 16, ""),
        "ERR result unavailable why=working\n"
    );
}

#[test]
fn a_persons_program_after_an_exit_is_not_the_unfinished_worker() {
    use crate::session_timeline::AgentStamp;
    let handle = crate::session_store::test_handle(17);
    let ctx = &handle.ctx;
    mark_session_task(ctx, 17, &TaskMark::new(TaskState::Working)).unwrap();
    let mut exit = TaskMark::new(TaskState::Exited);
    exit.why = Some(WHY_EXIT);
    mark_session_task(ctx, 17, &exit).unwrap();
    assert!(ctx.timeline.lock().unwrap().task().unwrap().unfinished_exit);
    crate::app_input::note_person(ctx, crate::human_input::Gesture::PlainEnter);
    let mut timeline = ctx.timeline.lock().unwrap();
    timeline.publish_agent(
        "busy",
        None,
        None,
        Some(aterm_phase::Program::Claude),
        AgentStamp {
            generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
            fp: 1,
        },
    );
    assert_eq!(timeline.task().unwrap().state, TaskState::Exited);
    assert!(!timeline.task().unwrap().unfinished_exit);
}

fn native(unix: &str) -> String {
    if cfg!(windows) {
        let swapped = unix.replace('/', "\\");
        if unix.starts_with('/') {
            format!("C:{swapped}")
        } else {
            swapped
        }
    } else {
        unix.to_string()
    }
}

fn tasks() -> PathBuf {
    PathBuf::from(native("/state/aterm/tasks"))
}

fn folder(rest: &str) -> String {
    native(&format!("/{rest}"))
}

fn parse(args: &str) -> Result<TaskMark, String> {
    TaskMark::parse(args, Some(&tasks()))
}

#[test]
fn states_and_origins_are_closed_words() {
    for state in TaskState::ALL {
        assert_eq!(TaskState::parse(state.as_str()), Some(state));
    }
    assert_eq!(TaskState::parse("Done"), None);
    assert_eq!(TaskState::parse("finished"), None);
    assert!(!TaskState::Working.is_terminal());
    assert!(TaskState::Done.is_terminal() && TaskState::Exited.is_terminal());
    assert!(TaskState::Stalled.is_terminal());

    assert!(valid_from("ctl") && valid_from(SID) && valid_from("s-ab"));
    for bad in [
        "",
        "s-",
        "s-xyz",
        "S-ab",
        "ctl2",
        "@s-ab",
        &format!("s-{}", "a".repeat(63)),
    ] {
        assert!(!valid_from(bad), "{bad:?}");
    }
}

/// The session log's closed task words name every word the record carries: each state, each
/// `why=`, and each rule a fact word can name (`fact_rule=`, so the `EVENT … task` row the log
/// rebuilds reads the word the record holds, never `-`).
#[test]
fn the_logs_task_words_mirror_the_task_record() {
    use aterm_agent::supervise::occupancy::RuleId;
    use aterm_slog::vocab::{FACT_RULE_WORDS, STATE_WORDS, WHY_WORDS, lookup};
    for state in TaskState::ALL {
        let word = match state {
            TaskState::Working => "working",
            TaskState::Done => "done",
            TaskState::Exited => "exited",
            TaskState::Stalled => "stalled",
            TaskState::Failed => "failed",
        };
        assert_eq!(state.as_str(), word);
        assert_eq!(lookup(STATE_WORDS, word), Some(word), "{state:?}");
    }
    for why in TASK_WHY {
        assert_eq!(lookup(WHY_WORDS, why), Some(*why), "why={why}");
    }
    for rule in RuleId::ALL {
        assert_eq!(
            lookup(FACT_RULE_WORDS, rule.name()),
            Some(rule.name()),
            "{rule:?}"
        );
    }
    assert_eq!(lookup(STATE_WORDS, "finished"), None);
    assert_eq!(lookup(WHY_WORDS, "because"), None);
    assert_eq!(lookup(FACT_RULE_WORDS, "hold-room"), None);
}

/// A STALL SAYS ITS CAUSE TO A PERSON: a worker refused as signed out says a person must sign
/// in, the one step nobody else can take; a failed install says so; a shell failure and a startup
/// stall keep "never started". Each launcher word is a mark's closed word too.
#[test]
fn a_stalls_words_name_a_sign_in_as_a_persons_step() {
    assert_eq!(
        stall_words(Some(WHY_SIGNIN)),
        "never started: signed out, a person must sign in"
    );
    assert_eq!(
        stall_words(Some(WHY_INSTALL)),
        "never started: its program could not be installed"
    );
    assert_eq!(stall_words(Some("shell")), "never started");
    assert_eq!(stall_words(Some("startup")), "never started");
    assert_eq!(
        stall_words(Some("held")),
        "held at a screen aterm does not know"
    );
    assert_eq!(stall_words(None), "stalled");
    for why in [WHY_SIGNIN, WHY_INSTALL] {
        assert_eq!(
            parse(&format!("stalled why={why}")).map(|m| m.why),
            Ok(Some(why))
        );
    }
}

#[test]
fn a_mark_parses_every_fact_and_names_what_it_refuses() {
    let path = tasks().join(format!("{SID}-1.txt"));
    let encoded = crate::control::pct_encode(&path.to_string_lossy());
    let mark = parse(&format!(
        " done from={SID} why=landed exit=0 result_bytes=12 result_path={encoded}"
    ))
    .expect("every fact");
    assert_eq!(
        mark,
        TaskMark {
            state: TaskState::Done,
            from: Some(SID.to_string()),
            why: Some("landed"),
            checked: None,
            left: None,
            exit: Some(0),
            result_bytes: 12,
            result_path: Some(path),
        }
    );
    assert_eq!(parse("stalled").unwrap(), TaskMark::new(TaskState::Stalled));
    for old in ["marker", "done-check"] {
        assert_eq!(
            parse(&format!("done why={old}")).unwrap().why,
            Some(old),
            "{old}"
        );
    }
    let spaced = tasks().join("a b.txt");
    assert_eq!(
        parse(&format!(
            "done result_path={}",
            crate::control::pct_encode(&spaced.to_string_lossy())
        ))
        .unwrap()
        .result_path,
        Some(spaced)
    );

    for usage in [
        "",
        "finished",
        "done extra",
        "done why=landed why=landed",
        "done exit=x",
        "done result_bytes=-1",
        "done color=red",
    ] {
        assert_eq!(parse(usage), Err(TASK_USAGE.to_string()), "{usage:?}");
    }
    assert!(
        parse("done from=s-xyz")
            .unwrap_err()
            .contains("from= is a session id")
    );
    assert!(parse("done why=because").unwrap_err().contains(
        "why= is one of landed|marker|done-check|exit|exit-status|no-exit-status|shell|startup|held|turn"
    ));
    let mut outsides: Vec<String> = [
        "/etc/passwd",
        "/state/aterm/tasks",
        "/state/aterm/tasks/../secrets",
        "/state/aterm/tasks/./x",
        "/state/aterm//tasks/x",
        "/state/aterm/tasks/x/",
        "state/aterm/tasks/x",
        "/state/aterm/tasks/a%0Ab",
    ]
    .map(native)
    .into();
    if cfg!(windows) {
        outsides.extend(
            [
                r"\state\aterm\tasks\x",
                "/state/aterm/tasks/x",
                r"D:\state\aterm\tasks\x",
                r"\\?\C:\state\aterm\tasks\x",
                "C:/state/aterm/tasks/./x",
                "C:/state/aterm//tasks/x",
                "C:/state/aterm/tasks/x/",
            ]
            .map(str::to_string),
        );
    }
    for outside in outsides {
        let err = parse(&format!("done result_path={outside}")).unwrap_err();
        assert!(
            err.contains("result_path= names a file under"),
            "{outside}: {err}"
        );
    }
    assert!(
        TaskMark::parse(
            &format!("done result_path={}", native("/state/aterm/tasks/x")),
            None
        )
        .is_err()
    );
    let dir = native("/state/aterm/tasks/");
    let longest = format!("{dir}{}", "a".repeat(RESULT_PATH_MAX - dir.len()));
    assert!(parse(&format!("done result_path={longest}")).is_ok());
    let err = parse(&format!("done result_path={longest}a")).unwrap_err();
    assert!(err.contains("result_path= names a file under"), "{err}");
}

#[test]
fn the_same_mark_twice_moves_nothing_and_a_change_bumps_the_rev() {
    let t0 = Instant::now();
    let mut working = TaskMark::new(TaskState::Working);
    working.from = Some(SID.to_string());
    let (first, moved) = working.apply(None, &Asks::default(), t0, 1_000);
    assert!(moved);
    assert_eq!(
        (first.rev, first.state, first.from.as_str()),
        (1, TaskState::Working, SID)
    );
    assert_eq!(first.since_unix_ms, 1_000);

    let (again, moved) = working.apply(
        Some(&first),
        &Asks::default(),
        t0 + Duration::from_secs(1),
        2_000,
    );
    assert!(!moved);
    assert_eq!(again, first, "no rev, no since moved");
    let (kept, moved) =
        TaskMark::new(TaskState::Working).apply(Some(&first), &Asks::default(), t0, 2_000);
    assert!(!moved && kept.from == SID);

    let mut done = TaskMark::new(TaskState::Done);
    done.why = Some("landed");
    done.result_bytes = 42;
    let (finished, moved) = done.apply(
        Some(&first),
        &Asks::default(),
        t0 + Duration::from_secs(2),
        3_000,
    );
    assert!(moved);
    assert_eq!((finished.rev, finished.state), (2, TaskState::Done));
    assert_eq!((finished.since_unix_ms, finished.result_bytes), (3_000, 42));
    assert_eq!(finished.from, SID, "the record keeps its origin");
    let (same, moved) = done.apply(
        Some(&finished),
        &Asks::default(),
        t0 + Duration::from_secs(9),
        9_000,
    );
    assert!(
        !moved && same.rev == 2,
        "a harness marking done again wakes no one"
    );

    let (resized, moved) = TaskMark::new(TaskState::Done).apply(
        Some(&finished),
        &Asks::default(),
        t0 + Duration::from_secs(3),
        4_000,
    );
    assert!(moved);
    assert_eq!((resized.rev, resized.since_unix_ms), (3, 3_000));
    assert_eq!((resized.why, resized.result_bytes), (None, 0));
    let (raised, moved) = TaskMark::new(TaskState::Done).apply(
        Some(&resized),
        &Asks::of_owners([1]),
        t0 + Duration::from_secs(4),
        5_000,
    );
    assert!(moved && raised.rev == 4 && raised.attention && raised.has_news());
    let bare = TaskMark::new(TaskState::Done);
    let (asked, moved) = bare.apply(Some(&raised), &Asks::of_owners([1, 2]), t0, 6_000);
    assert!(
        moved && asked.rev == 5,
        "a second ask beside the first is news"
    );
    let (fewer, moved) = bare.apply(Some(&asked), &Asks::of_owners([2]), t0, 7_000);
    assert!(
        !moved && fewer.rev == 5 && fewer.asks == Asks::of_owners([2]),
        "one of two taken down is none, and the record keeps the ask that stands"
    );
}

/// A MAC ASLEEP BEFORE AN UPDATE SPENDS NONE OF A CARRIED CLOCK: the successor's `since` and
/// `started` stand as long before its now as the predecessor's monotonic clock measured them,
/// not the wall clock's whole gap, which still caps them. CONTROL: a row without the ages, an
/// older build's, goes by the wall clock.
#[test]
fn a_carried_record_keeps_only_the_time_it_was_awake() {
    let t0 = Instant::now();
    let record = TaskRecord {
        rev: 3,
        state: TaskState::Stalled,
        from: "ctl".into(),
        manager_node: None,
        told: Default::default(),
        why: Some("held"),
        checked: None,
        left: None,
        exit: None,
        finished: false,
        unfinished_exit: false,
        result_bytes: 0,
        result_path: None,
        attention: false,
        asks: Asks::default(),
        since_unix_ms: 10_000,
        since: t0 + Duration::from_secs(6),
        started_unix_ms: 4_000,
        started: t0,
        entered: 3,
        carried_entered: None,
        agent: None,
        program: None,
        trusted_cwd: None,
        fact: None,
        program_block: None,
        fact_rev: 0,
    };
    let row = record.carry_row(t0 + Duration::from_secs(66));
    assert!(row.contains(" age_ms=60000 ran_ms=66000"), "{row}");
    let asleep_ms = 600_000;
    let after = t0 + Duration::from_secs(2_000);
    let back = TaskRecord::from_carry_row(&row, None, after, 70_000 + asleep_ms).expect("reads");
    assert_eq!(back.since, after - Duration::from_secs(60));
    assert_eq!(back.started, after - Duration::from_secs(66));
    assert_eq!((back.since_unix_ms, back.started_unix_ms), (10_000, 4_000));

    let older = row.replace(" age_ms=60000 ran_ms=66000", "");
    let back = TaskRecord::from_carry_row(&older, None, after, 70_000 + asleep_ms).expect("reads");
    assert_eq!(back.since, after - Duration::from_secs(660));
    assert_eq!(back.started, after - Duration::from_secs(666));

    let odd = row.replace(
        " age_ms=60000 ran_ms=66000",
        " age_ms=99999999 ran_ms=99999999",
    );
    let back = TaskRecord::from_carry_row(&odd, None, after, 70_000).expect("reads");
    assert_eq!(
        back.since,
        after - Duration::from_secs(60),
        "no more than the wall clock's gap"
    );
    assert_eq!(back.started, after - Duration::from_secs(66));
    for bad in ["age_ms=x", "ran_ms=-1"] {
        let row = format!("{older} {bad}");
        assert_eq!(
            TaskRecord::from_carry_row(&row, None, after, 70_000),
            None,
            "{bad}"
        );
    }
}

#[test]
fn the_carried_row_round_trips_and_a_bad_row_is_none() {
    let t0 = Instant::now();
    let record = TaskRecord {
        rev: 7,
        state: TaskState::Exited,
        from: SID.to_string(),
        manager_node: None,
        told: Default::default(),
        why: Some("exit"),
        checked: None,
        left: None,
        exit: Some(-2),
        finished: false,
        unfinished_exit: false,
        result_bytes: 9,
        result_path: Some(tasks().join("r 1.txt")),
        attention: true,
        asks: Asks::default(),
        since_unix_ms: 10_000,
        since: t0,
        started_unix_ms: 10_000,
        started: t0,
        entered: 7,
        carried_entered: None,
        agent: Some(aterm_phase::Program::Agy),
        program: None,
        trusted_cwd: Some(folder("w/my proj")),
        fact: None,
        program_block: None,
        fact_rev: 0,
    };
    let carried = t0 + Duration::from_secs(5);
    let row = record.carry_row(carried);
    let (wire, trusted) = if cfg!(windows) {
        (r"C:\state\aterm\tasks\r%201.txt", r"C:\w\my%20proj")
    } else {
        ("/state/aterm/tasks/r%201.txt", "/w/my%20proj")
    };
    assert_eq!(
        row,
        format!(
            "rev=7 state=exited from={SID} since=10000 result_bytes=9 attention=1 why=exit \
             exit=-2 result_path={wire} age_ms=5000 agent=agy trusted_cwd={trusted}"
        )
    );
    let later = t0 + Duration::from_secs(60);
    let back = TaskRecord::from_carry_row(&row, Some(&tasks()), later, 15_000).expect("reads");
    assert_eq!(back.since, later - Duration::from_secs(5));
    assert_eq!(
        back.carry_row(later),
        row,
        "the row reads back to its own bytes"
    );
    let again = TaskRecord {
        since: t0,
        started: t0,
        ..back
    };
    assert_eq!(again, record);
    let started_earlier = TaskRecord {
        started_unix_ms: 4_000,
        started: t0
            .checked_sub(Duration::from_secs(6))
            .expect("a clock past six seconds"),
        ..record.clone()
    };
    let back = TaskRecord::from_carry_row(
        &started_earlier.carry_row(carried),
        Some(&tasks()),
        later,
        15_000,
    )
    .expect("reads");
    assert_eq!(back.started_unix_ms, 4_000);
    assert_eq!(back.started, later - Duration::from_secs(11));
    let bare = TaskRecord {
        why: None,
        exit: None,
        result_path: None,
        attention: false,
        agent: None,
        program: None,
        trusted_cwd: None,
        ..record.clone()
    };
    let back = TaskRecord::from_carry_row(&bare.carry_row(t0), Some(&tasks()), t0, 10_000).unwrap();
    assert_eq!(back, bare);
    let held = TaskRecord {
        state: TaskState::Stalled,
        why: Some("held"),
        ..bare.clone()
    };
    assert_eq!(
        TaskRecord::from_carry_row(&held.carry_row(t0), Some(&tasks()), t0, 10_000),
        Some(held)
    );
    let newer = format!("{} model=opus pinned", bare.carry_row(t0));
    assert_eq!(
        TaskRecord::from_carry_row(&newer, Some(&tasks()), t0, 10_000),
        Some(bare.clone())
    );
    for agent in aterm_phase::Program::ALL {
        let with = TaskRecord {
            agent: Some(agent),
            ..bare.clone()
        };
        let back = TaskRecord::from_carry_row(&with.carry_row(t0), Some(&tasks()), t0, 10_000)
            .expect("reads");
        // A worker's agent is one a spawn starts, or a program worker's `generic`.
        let spawned = agent == aterm_phase::Program::Generic
            || aterm_agent::harness::worker::worker_cli(agent).is_some();
        assert_eq!(back.agent, spawned.then_some(agent), "{agent:?}");
        assert_eq!(
            TaskRecord {
                agent: None,
                ..back
            },
            bare,
            "{agent:?}"
        );
    }
    let with = TaskRecord {
        trusted_cwd: Some(folder("w/proj")),
        ..bare.clone()
    };
    assert_eq!(
        TaskRecord::from_carry_row(&with.carry_row(t0), Some(&tasks()), t0, 10_000),
        Some(with.clone())
    );
    for bad in ["w/proj", "/w/../etc", "/w/proj/", "/w/a%0Ab", "/w//proj"] {
        let row = format!("{} trusted_cwd={bad}", bare.carry_row(t0));
        assert_eq!(
            TaskRecord::from_carry_row(&row, Some(&tasks()), t0, 10_000),
            Some(bare.clone()),
            "{bad}"
        );
    }
    let unknown = format!("{} agent=a-later-cli", bare.carry_row(t0));
    assert_eq!(
        TaskRecord::from_carry_row(&unknown, Some(&tasks()), t0, 10_000),
        Some(bare.clone())
    );
    for retired in aterm_phase::RETIRED_AGENTS {
        let carried = format!("{} agent={retired}", bare.carry_row(t0));
        assert_eq!(
            TaskRecord::from_carry_row(&carried, Some(&tasks()), t0, 10_000),
            Some(bare.clone()),
            "a worker of the retired {retired} crosses as no agent's"
        );
    }

    for bad in [
        "",
        "rev=0 state=done from=ctl since=1 result_bytes=0 attention=0",
        "rev=1 state=finished from=ctl since=1 result_bytes=0 attention=0",
        "rev=1 state=done from=me since=1 result_bytes=0 attention=0",
        "rev=1 state=done from=ctl result_bytes=0 attention=0",
        "rev=1 state=done from=ctl since=1 result_bytes=0 attention=2",
        "rev=1 state=done from=ctl since=1 result_bytes=0 attention=0 why=because",
        "rev=1 state=done from=ctl since=1 result_bytes=0 attention=0 rev=2",
        "rev=1 state=done from=ctl since=1 result_bytes=0 attention=0 color=red color=blue",
        "rev=1 state=done from=ctl since=1 result_bytes=0 attention=0 result_path=/etc/passwd",
    ] {
        assert_eq!(
            TaskRecord::from_carry_row(bad, Some(&tasks()), t0, 1),
            None,
            "{bad:?}"
        );
    }
}

#[test]
fn the_reply_event_and_status_spellings() {
    let record = TaskRecord {
        rev: 3,
        state: TaskState::Done,
        from: "ctl".into(),
        manager_node: None,
        told: Default::default(),
        why: Some("landed"),
        checked: None,
        left: None,
        exit: None,
        finished: false,
        unfinished_exit: false,
        result_bytes: 120,
        result_path: None,
        attention: false,
        asks: Asks::default(),
        since_unix_ms: 0,
        since: Instant::now(),
        started_unix_ms: 0,
        started: Instant::now(),
        entered: 3,
        carried_entered: None,
        agent: Some(aterm_phase::Program::Agy),
        program: None,
        trusted_cwd: Some(folder("w")),
        fact: None,
        program_block: None,
        fact_rev: 0,
    };
    let fields = record.wire_fields();
    assert!(
        fields.starts_with("rev=3 state=done from=ctl since_ms="),
        "{fields}"
    );
    assert!(
        fields.ends_with(" result_bytes=120 conclusion=finished why=landed"),
        "{fields}"
    );
    assert_eq!(status_tokens(Some(&record)), "task_state=done task_rev=3");
    assert_eq!(status_tokens(None), "task_state=- task_rev=0");
    assert_eq!(
        await_row(SID, Some(&record), "idle", "-", "-", false),
        format!(
            "@{SID} rev=3 state=done agent=idle program_state=- attention=- result_bytes=120 \
             conclusion=finished why=landed"
        )
    );
    assert_eq!(
        await_row(
            SID,
            None,
            "unknown",
            "blocked:permission",
            "stuck%20on%20CI",
            false
        ),
        format!(
            "@{SID} rev=0 state=- agent=unknown program_state=blocked:permission \
             attention=stuck%20on%20CI result_bytes=0"
        )
    );
    assert!(await_row(SID, Some(&record), "-", "-", "-", true).contains(" state=closed "));
    // A closed row says its record's own state: a task that concluded says the state it ended
    // in; one closed while working says so, its `failed` the closed tab's reading alone.
    assert!(
        await_row(SID, Some(&record), "-", "-", "-", true)
            .ends_with(" result_bytes=120 task_state=done conclusion=finished why=landed")
    );
    let working = TaskRecord {
        state: TaskState::Working,
        why: None,
        ..record.clone()
    };
    assert!(
        await_row(SID, Some(&working), "-", "-", "-", true)
            .ends_with(" result_bytes=120 task_state=working conclusion=failed"),
        "{}",
        await_row(SID, Some(&working), "-", "-", "-", true)
    );
    assert!(
        !await_row(SID, Some(&working), "-", "-", "-", false).contains("task_state="),
        "a live row's state is its record's"
    );
}

fn words(line: &str) -> Vec<&str> {
    line.split_whitespace().collect()
}

#[test]
fn await_task_takes_its_list_combine_since_and_timeout() {
    let all = parse_await_task(&words("@s-a:4,@2,@. all since=3 timeout=1500"), true).unwrap();
    assert_eq!(
        all,
        AwaitTaskArgs {
            targets: vec![
                TargetArg {
                    selector: "s-a".into(),
                    since: Some(4),
                    fact_since: None,
                },
                TargetArg {
                    selector: "2".into(),
                    since: None,
                    fact_since: None,
                },
                TargetArg {
                    selector: ".".into(),
                    since: None,
                    fact_since: None,
                },
            ],
            combine: Combine::All,
            since: Some(3),
            facts: false,
            fact_since: None,
            timeout_ms: Some(1500),
        }
    );
    let facts = parse_await_task(&words("@s-a:4:2,@s-b:5 facts=1 fact_since=1"), true).unwrap();
    assert_eq!(
        (
            facts.facts,
            facts.fact_since,
            facts
                .targets
                .iter()
                .map(|t| (t.since, t.fact_since))
                .collect::<Vec<_>>()
        ),
        (true, Some(1), vec![(Some(4), Some(2)), (Some(5), None)])
    );
    let single = parse_await_task(&words("since=2 facts=1"), false).unwrap();
    assert_eq!((single.facts, single.fact_since), (true, None));
    let single = parse_await_task(&words("timeout 999999999"), false).unwrap();
    assert_eq!(single.combine, Combine::Any);
    assert_eq!(single.timeout_ms, Some(aterm_control::wire::WAIT_MAX_MS));
    assert_eq!(parse_await_task(&[], false).unwrap().targets, Vec::new());
    assert_eq!(
        parse_await_task(&words("any"), false).unwrap().combine,
        Combine::Any
    );

    for usage in [
        "@s-a",
        "since=x",
        "since=1 since=2",
        "any all",
        "timeout=soon",
        "timeout",
        "later",
        "facts=0",
        "facts=1 facts=1",
        "fact_since=2",
        "facts=1 fact_since=x",
        "facts=1 fact_since=1 fact_since=2",
    ] {
        assert_eq!(
            parse_await_task(&words(usage), false),
            Err(AWAIT_TASK_USAGE.to_string()),
            "{usage:?}"
        );
    }
    for usage in [
        "@s-a @s-b",
        "@s-a:x",
        "@s-a,s-b",
        "@s-a:",
        "@s-a:1:2",
        "@s-a:1: facts=1",
        "@s-a:1:x facts=1",
        "@s-a::2 facts=1",
        "@s-a:1:2:3 facts=1",
    ] {
        assert_eq!(
            parse_await_task(&words(usage), true),
            Err(AWAIT_TASK_USAGE.to_string()),
            "{usage:?}"
        );
    }
    for empty in ["@", "@s-a,@", "@,@s-b", "@:3", "@s-a,@:2"] {
        assert_eq!(
            parse_await_task(&words(empty), true),
            Err(format!("{}\n", aterm_types::control_verbs::EMPTY_SELECTOR)),
            "{empty:?}"
        );
    }
    assert_eq!(
        parse_await_task(&words("@s-a,@."), true)
            .map(|args| args.targets.len())
            .as_ref(),
        Ok(&2)
    );
    let crowd = (0..=MAX_AWAIT_TARGETS)
        .map(|i| format!("@{i}"))
        .collect::<Vec<_>>()
        .join(",");
    assert!(
        parse_await_task(&[crowd.as_str()], true)
            .unwrap_err()
            .contains("too many await task targets")
    );
}

#[test]
fn a_mark_records_its_row_and_the_supervisors_attention_moves_the_rev() {
    use crate::session_timeline::{MetaEdit, write_attention_owned};

    let handle = crate::session_store::test_handle(4);
    let owner = crate::harness_host::ATTENTION_OWNER;
    write_attention_owned(
        &handle.ctx,
        owner,
        MetaEdit::Set("needs a login"),
        aterm_slog::Actor::Harness,
    )
    .unwrap();
    assert!(handle.ctx.timeline.lock().unwrap().task().is_none());
    write_attention_owned(
        &handle.ctx,
        owner,
        MetaEdit::Clear,
        aterm_slog::Actor::Harness,
    )
    .unwrap();

    let rows = |ctx: &crate::SessionCtx| -> Vec<String> {
        ctx.timeline
            .lock()
            .unwrap()
            .since(None)
            .filter(|e| e.kind == "task-change")
            .map(|e| e.payload.to_string())
            .collect()
    };
    let (record, moved) =
        mark_session_task(&handle.ctx, 4, &TaskMark::new(TaskState::Working)).expect("unfenced");
    assert!(moved && record.rev == 1 && !record.attention);
    let (_, moved) =
        mark_session_task(&handle.ctx, 4, &TaskMark::new(TaskState::Working)).expect("unfenced");
    assert!(!moved);
    assert_eq!(rows(&handle.ctx), ["rev=1 state=working attention=0"]);
    assert_eq!(
        crate::subscribe::timeline_wire_kind("task-change"),
        Some("task"),
        "the digest pushes the row as `EVENT <local> task …`"
    );

    assert!(
        write_attention_owned(
            &handle.ctx,
            owner,
            MetaEdit::Set("needs a login"),
            aterm_slog::Actor::Harness
        )
        .unwrap()
    );
    write_attention_owned(
        &handle.ctx,
        "human",
        MetaEdit::Set("lunch"),
        aterm_slog::Actor::Human,
    )
    .unwrap();
    assert!(
        write_attention_owned(
            &handle.ctx,
            owner,
            MetaEdit::Clear,
            aterm_slog::Actor::Harness
        )
        .unwrap()
    );
    assert_eq!(
        rows(&handle.ctx),
        [
            "rev=1 state=working attention=0",
            "rev=2 state=working attention=1",
            "rev=3 state=working attention=0",
        ]
    );
    write_attention_owned(
        &handle.ctx,
        owner,
        MetaEdit::Set("a dialog"),
        aterm_slog::Actor::Harness,
    )
    .unwrap();
    let (record, _) =
        mark_session_task(&handle.ctx, 4, &TaskMark::new(TaskState::Stalled)).expect("unfenced");
    assert_eq!((record.rev, record.attention), (5, true));
}

#[test]
fn a_workers_needs_you_entry_raises_the_task_records_attention() {
    use crate::session_timeline::{MetaEdit, write_attention_kind};
    let handle = crate::session_store::test_handle(4);
    let ctx = &handle.ctx;
    let needs = crate::harness_host::NEEDS_ATTENTION_OWNER;
    let supervisor = crate::harness_host::ATTENTION_OWNER;
    let ask = Some(aterm_types::needs::AttentionKind::Ask);
    let write = |owner: &str, edit: MetaEdit<'_>| {
        write_attention_kind(ctx, owner, edit, ask, aterm_slog::Actor::Harness).unwrap();
    };
    let flag = || {
        let r = ctx
            .timeline
            .lock()
            .unwrap()
            .task()
            .cloned()
            .expect("a task");
        (r.rev, r.attention)
    };
    assert!(crate::task_record::owner_moves_task(needs));
    assert!(!crate::task_record::owner_moves_task("human"));
    mark_session_task(ctx, 4, &TaskMark::new(TaskState::Working)).expect("unfenced");
    assert_eq!(flag(), (1, false));
    write(needs, MetaEdit::Set("a release credential"));
    assert_eq!(flag(), (2, true), "NEEDS YOU raises it");
    write(supervisor, MetaEdit::Set("a dialog"));
    assert_eq!(
        flag(),
        (3, true),
        "a second ask under the standing one is news"
    );
    write(needs, MetaEdit::Clear);
    assert_eq!(
        flag(),
        (3, true),
        "the supervisor's entry still stands, and an ask taken down from two is none"
    );
    write(supervisor, MetaEdit::Clear);
    assert_eq!(flag(), (4, false), "both cleared");
    let codex = crate::task_record::SELF_CLAIM_ATTENTION_OWNER;
    assert!(crate::task_record::owner_moves_task(codex));
    write(codex, MetaEdit::Set("box: Allow this command?"));
    assert_eq!(
        flag(),
        (5, true),
        "a point codex-alab left to a person raises it"
    );
    write(codex, MetaEdit::Clear);
    assert_eq!(flag(), (6, false), "and its clearing lowers it");
    let before = flag();
    write_attention_kind(
        ctx,
        "human",
        MetaEdit::Set("lunch"),
        None,
        aterm_slog::Actor::Human,
    )
    .unwrap();
    assert_eq!(flag(), before, "a person's entry is not the task's news");
}

/// THE SAME NEEDS ENTRY IS NO NEWS WHETHER ITS NEEDS ARE KNOWN OR NOT ([`Asks::joins`]): with no
/// told set describing it (a record a seamless update carried, a proven self-claim's set gone
/// with its claim) the entry asks its words alone, and a set describing the same words told
/// after, or the set that did gone, joins nothing. CONTROL: beside a told set a need joining is
/// news though the words hold and one taken down is none, new words no set describes are news,
/// and an entry raised is news where its clearing is none.
#[test]
fn the_same_needs_entry_is_no_news_with_its_needs_known_or_not() {
    let owner = crate::harness_host::NEEDS_ATTENTION_OWNER;
    let one = "claude needs you: sign 0.48.0 — carrying on";
    let two = "claude needs you: sign 0.48.0 (+1 more) — carrying on";
    let asks = |entry: Option<&str>, told: &[&str]| {
        let mut meta = crate::session_timeline::SessionMeta::default();
        let mut set = crate::needs::ToldNeeds::default();
        if let Some(entry) = entry {
            meta.set_attention_kind(
                owner,
                Some(entry.to_string()),
                Some(aterm_types::needs::AttentionKind::Ask),
            )
            .expect("room for the entry");
            let told: Vec<String> = told.iter().map(|need| (*need).to_string()).collect();
            let held = crate::needs::HarnessNeeds::told(&told, entry).map(std::sync::Arc::new);
            set.tell(held, Some(entry));
        }
        supervisor_asks(&meta, &set)
    };
    let words = asks(Some(one), &[]);
    let known = asks(Some(one), &["sign 0.48.0"]);
    assert_ne!(
        words, known,
        "a told set's needs are asked, not its entry's words"
    );
    assert!(
        !known.joins(&words),
        "the set told after a carry is no news"
    );
    assert!(
        !words.joins(&known),
        "the set gone with its claim is no news"
    );

    let both = asks(Some(two), &["sign 0.48.0", "grant Full Disk Access"]);
    assert!(
        both.joins(&known) && both.joins(&words),
        "CONTROL: a need joins"
    );
    assert!(
        !known.joins(&both),
        "CONTROL: one of two taken down is none"
    );
    let swapped = asks(Some(two), &["sign 0.48.0", "restart the Mac"]);
    assert!(
        swapped.joins(&both),
        "CONTROL: a need joins under the same words"
    );
    assert!(
        asks(Some(two), &[]).joins(&known),
        "CONTROL: new words no set describes are news"
    );
    let none = asks(None, &[]);
    assert!(none.is_empty() && !words.is_empty());
    assert!(
        words.joins(&none) && !none.joins(&words),
        "CONTROL: raised is news, cleared none"
    );
}

/// A WAIT IS NO TASK NEWS: `await task` wakes on a worker done, exited, stalled or needing a
/// person, and a wait it sits out is none of those. So a `kind=wait` entry leaves the record's
/// attention down and its rev where it stood, under codex-alab's own wait owner
/// (`codex-alab-wait`, no task owner) and under the task owners alike (aterm's harness waiting
/// out a limit under `supervisor`, a wait under `codex-alab` or `needs`); an ask turned into a
/// wait takes the attention down. NEGATIVE CONTROL: the same words as an ask under a task owner
/// raise it and move the rev, and `codex-alab-wait` moves nothing whatever its kind.
#[test]
fn a_kind_wait_entry_never_raises_the_task_records_attention() {
    use crate::session_timeline::{MetaEdit, write_attention_kind};
    use aterm_types::needs::AttentionKind;
    let handle = crate::session_store::test_handle(4);
    let ctx = &handle.ctx;
    let write = |owner: &str, edit: MetaEdit<'_>, kind: Option<AttentionKind>| {
        write_attention_kind(ctx, owner, edit, kind, aterm_slog::Actor::Harness).unwrap();
    };
    let flag = || {
        let r = ctx
            .timeline
            .lock()
            .unwrap()
            .task()
            .cloned()
            .expect("a task");
        (r.rev, r.attention)
    };
    let (wait, ask) = (Some(AttentionKind::Wait), Some(AttentionKind::Ask));
    let words = "waiting out the usage limit \u{b7} resets 15:00";
    let codex_wait = "codex-alab-wait";
    let supervisor = crate::harness_host::ATTENTION_OWNER;
    let owners = [
        codex_wait,
        crate::task_record::SELF_CLAIM_ATTENTION_OWNER,
        supervisor,
        crate::harness_host::NEEDS_ATTENTION_OWNER,
    ];
    assert!(!crate::task_record::owner_moves_task(codex_wait));
    mark_session_task(ctx, 4, &TaskMark::new(TaskState::Working)).expect("unfenced");
    let before = flag();
    assert_eq!(before, (1, false));
    for owner in owners {
        write(owner, MetaEdit::Set(words), wait);
        assert_eq!(flag(), before, "{owner}'s wait is no news");
    }
    for owner in owners {
        write(owner, MetaEdit::Clear, None);
    }
    assert_eq!(flag(), before, "and its clearing is none");

    write(codex_wait, MetaEdit::Set(words), ask);
    assert_eq!(flag(), before, "codex-alab-wait is no task owner");
    write(codex_wait, MetaEdit::Clear, None);
    write(supervisor, MetaEdit::Set(words), ask);
    assert_eq!(flag(), (2, true), "the same limit as an ask raises it");
    write(supervisor, MetaEdit::Set(words), wait);
    assert_eq!(
        flag(),
        (3, false),
        "the ask turned into a wait takes it down"
    );
    write(
        crate::task_record::SELF_CLAIM_ATTENTION_OWNER,
        MetaEdit::Set("box: Allow this command?"),
        None,
    );
    assert_eq!(
        flag(),
        (4, true),
        "codex-alab's ask, no kind named, raises it"
    );
}

#[test]
fn the_task_verb_reads_marks_and_refuses_an_edge_the_mark() {
    let handle = crate::session_store::test_handle(5);
    let ctx = &handle.ctx;
    assert_eq!(cmd_task(ctx, 5, Scope::Owner, ""), "OK task -\n");
    assert_eq!(cmd_task(ctx, 5, Scope::Owner, "mark"), TASK_USAGE);
    assert_eq!(cmd_task(ctx, 5, Scope::Owner, "status"), TASK_USAGE);
    assert_eq!(cmd_task(ctx, 5, Scope::Owner, "marked done"), TASK_USAGE);
    let edge = Scope::Edge(aterm_session::EdgeToken::generate());
    assert_eq!(cmd_task(ctx, 5, edge, "mark done"), "ERR denied\n");
    assert!(
        ctx.timeline.lock().unwrap().task().is_none(),
        "the edge marked nothing"
    );

    let marked = cmd_task(ctx, 5, Scope::Owner, &format!("mark working from={SID}"));
    assert!(
        marked.starts_with(&format!("OK task rev=1 state=working from={SID} since_ms=")),
        "{marked}"
    );
    assert!(
        marked.ends_with(" result_bytes=0 agent=- program_state=- attention=- changed=1\n"),
        "{marked}"
    );
    let again = cmd_task(ctx, 5, Scope::Owner, "mark working");
    assert!(
        again.contains(" rev=1 ") && again.ends_with(" changed=0\n"),
        "{again}"
    );
    ctx.meta
        .lock()
        .unwrap()
        .set("attention", Some("stuck on CI".into()));
    let done = cmd_task(ctx, 5, Scope::Owner, "mark done why=landed result_bytes=7");
    assert!(done.contains(" rev=2 state=done "), "{done}");
    let read = cmd_task(ctx, 5, edge, "");
    assert!(read.starts_with("OK task rev=2 state=done from="), "{read}");
    assert!(
        read.ends_with(
            " result_bytes=7 conclusion=finished why=landed agent=- program_state=- \
             attention=stuck%20on%20CI\n"
        ),
        "{read}"
    );
}

/// ONLY ATERM'S OWN LOOP WRITES `checked=` (the post-landing review, 2026-11-01): a hand mark
/// from the owner socket (the instance token, [`Principal::Instance`]) naming it is refused and
/// marks nothing; the same mark from the harness's connection ([`Principal::Harness`]) keeps it.
/// CONTROL: the hand mark without `checked=` marks done.
#[test]
fn only_the_harness_writes_checked_and_a_hand_mark_naming_it_is_refused() {
    use crate::control::{Principal, ServingPrincipal};
    let handle = crate::session_store::test_handle(7);
    let ctx = &handle.ctx;
    let on = "3f9a1c2e4b7d@origin/main";
    let hand = format!("mark done why=landed checked={on}");
    {
        let _hand = ServingPrincipal::enter(Principal::Instance);
        assert_eq!(cmd_task(ctx, 7, Scope::Owner, &hand), CHECKED_BY_HAND);
        assert!(
            ctx.timeline.lock().unwrap().task().is_none(),
            "the refused mark marked nothing"
        );
        let plain = cmd_task(ctx, 7, Scope::Owner, "mark done why=landed");
        assert!(
            plain.contains(" state=done ") && !plain.contains("checked="),
            "CONTROL: {plain}"
        );
    }
    let _harness = ServingPrincipal::enter(Principal::Harness);
    let checked = cmd_task(ctx, 7, Scope::Owner, &hand);
    assert!(checked.contains(&format!(" checked={on}")), "{checked}");
}

#[test]
fn a_done_over_a_deadline_stall_takes_its_attention_down_in_the_same_rev() {
    let handle = crate::session_store::test_handle(6);
    let ctx = &handle.ctx;
    let stalled_held = TaskMark {
        why: Some("held"),
        ..TaskMark::new(TaskState::Stalled)
    };
    let raise = |text: &str| {
        mark_session_task_with(
            ctx,
            6,
            Some(&stalled_held),
            AttentionEdit::Raise(text),
            Duration::ZERO,
            |_, _| true,
        )
        .expect("unfenced")
        .and_then(|m| m.record)
        .expect("a record")
    };
    let held = format!(
        "{}nothing moved for 300 s",
        aterm_agent::supervise::HELD_HEAD
    );
    let stall = raise(&held);
    assert!(stall.attention);
    let done = cmd_task(ctx, 6, Scope::Owner, "mark done why=landed result_bytes=7");
    assert!(
        done.contains(&format!(" rev={} state=done ", stall.rev + 1)),
        "{done}"
    );
    assert!(done.ends_with(" attention=- changed=1\n"), "{done}");
    let after = ctx
        .timeline
        .lock()
        .unwrap()
        .task()
        .cloned()
        .expect("a record");
    assert_eq!((after.rev, after.attention), (stall.rev + 1, false));
    let over = mark_session_task_with(
        ctx,
        6,
        None,
        AttentionEdit::TakeDown(crate::harness_host::deadline_attention_stands),
        Duration::ZERO,
        |_, _| true,
    )
    .expect("unfenced")
    .expect("applied");
    assert!(!over.moved && !over.attention_moved, "{over:?}");

    let stall = raise(&held);
    let again = cmd_task(ctx, 6, Scope::Owner, "mark stalled why=held");
    assert!(
        again.contains(&format!(" attention={}", crate::control::pct_encode(&held))),
        "{again}"
    );
    assert_eq!(
        ctx.timeline.lock().unwrap().task().map(|r| r.rev),
        Some(stall.rev)
    );
    raise("a usage limit until 5pm");
    let done = cmd_task(ctx, 6, Scope::Owner, "mark done why=landed");
    assert!(done.contains(" attention=a%20usage%20limit"), "{done}");
}

#[test]
fn a_result_path_is_judged_lexically() {
    let dir = native("/nonexistent/tasks");
    let dir = Some(Path::new(&dir));
    let inside = native("/nonexistent/tasks/a/b.txt");
    assert_eq!(result_path(&inside, dir), Some(PathBuf::from(&inside)));
    assert_eq!(result_path(&native("/nonexistent/tasksx/b.txt"), dir), None);
}

/// A result under `%LOCALAPPDATA%\aterm\tasks` is judged under it, a space travelling
/// pct-encoded, and kept as spelled; so is one under a state root spelled with `/`
/// (`ATERM_STATE_HOME=C:/…`), which `tasks\<file>` joins with `\`.
#[cfg(windows)]
#[test]
fn a_windows_result_path_is_judged_under_its_state_dir() {
    let state = Path::new(r"C:\Users\x\AppData\Local\aterm");
    let tasks = tasks_dir_under(state);
    let file = tasks.join("r 1.txt");
    assert_eq!(
        file.to_str(),
        Some(r"C:\Users\x\AppData\Local\aterm\tasks\r 1.txt")
    );
    let encoded = crate::control::pct_encode(&file.to_string_lossy());
    assert_eq!(encoded, r"C:\Users\x\AppData\Local\aterm\tasks\r%201.txt");
    assert_eq!(result_path(&encoded, Some(&tasks)), Some(file.clone()));
    let mark = TaskMark::parse(&format!("done result_path={encoded}"), Some(&tasks))
        .expect("a Windows path under <state>\\tasks");
    assert_eq!(mark.result_path, Some(file));

    let slashed = tasks_dir_under(Path::new("C:/Users//x/state"));
    let mixed = slashed.join("s-1-1.result.md");
    assert_eq!(
        mixed.to_str(),
        Some(r"C:/Users//x/state\tasks\s-1-1.result.md")
    );
    assert_eq!(
        result_path(&mixed.to_string_lossy(), Some(&slashed)),
        Some(mixed.clone())
    );
    assert_eq!(
        result_path("C:/Users//x/state/tasks/s-1-1.result.md", Some(&slashed)),
        Some(PathBuf::from("C:/Users//x/state/tasks/s-1-1.result.md"))
    );

    for outside in [
        r"C:Users\x\AppData\Local\aterm\tasks\r.txt",
        r"D:\Users\x\AppData\Local\aterm\tasks\r.txt",
        r"C:\Users\x\AppData\Local\aterm\tasksx\r.txt",
        r"C:\Users\x\AppData\Local\aterm\tasks\..\r.txt",
    ] {
        assert_eq!(result_path(outside, Some(&tasks)), None, "{outside}");
    }
}

#[test]
fn the_result_verb_answers_the_snapshot_the_record_names() {
    let handle = crate::session_store::test_handle(6);
    let ctx = &handle.ctx;
    assert_eq!(cmd_result(ctx, 6, ""), "ERR no task\n");
    let mark = |mark: TaskMark| {
        mark_session_task(ctx, 6, &mark).expect("unfenced");
    };
    mark(TaskMark::new(TaskState::Working));
    assert_eq!(
        cmd_result(ctx, 6, ""),
        "ERR result unavailable why=working\n"
    );
    assert_eq!(
        cmd_result(ctx, 6, "full"),
        "ERR result unavailable why=working\n"
    );
    assert_eq!(cmd_result(ctx, 6, "everything"), RESULT_USAGE);
    mark(TaskMark::new(TaskState::Stalled));
    assert_eq!(
        cmd_result(ctx, 6, ""),
        "ERR result unavailable why=stalled\n"
    );
    mark(TaskMark::new(TaskState::Done));
    assert_eq!(
        cmd_result(ctx, 6, ""),
        "ERR result unavailable why=no-snapshot\n"
    );

    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-task-result-")
        .tempdir()
        .expect("scratch");
    let transcript = dir.path().join("t.jsonl");
    let row = r#"{"type":"assistant","isSidechain":false,"uuid":"a-1","sessionId":"0b22","message":{"id":"msg_1","model":"claude-sonnet-5-5","content":[{"type":"text","text":"Fixed the parser."}]}}"#;
    let snap = aterm_agent::harness::result::snapshot(
        &dir.path().join("tasks"),
        "s-6-1",
        Some((&transcript, &format!("{row}\n"))),
        aterm_agent::harness::result::DoneBy::Turn,
    )
    .expect("written");
    let done = TaskMark {
        why: Some("landed"),
        result_bytes: snap.bytes,
        result_path: Some(snap.path.clone()),
        ..TaskMark::new(TaskState::Done)
    };
    mark(done.clone());
    let answer = "OK 1 source=transcript session=0b22 uuid=a-1 bytes=17\nFixed the parser.\n";
    assert_eq!(cmd_result(ctx, 6, ""), answer);
    mark(TaskMark {
        state: TaskState::Exited,
        why: Some("exit"),
        exit: Some(0),
        ..done
    });
    assert_eq!(cmd_result(ctx, 6, ""), answer);
    mark(TaskMark::new(TaskState::Exited));
    assert_eq!(
        cmd_result(ctx, 6, ""),
        "ERR result unavailable why=exited\n"
    );
}

#[test]
fn result_reads_a_missed_reply_of_its_turn_again_and_keeps_it() {
    let handle = crate::session_store::test_handle(6);
    let ctx = &handle.ctx;
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-task-reread-")
        .tempdir()
        .expect("scratch");
    let transcript = dir.path().join("t.jsonl");
    let prompt = r#"{"type":"user","isSidechain":false,"uuid":"u-1","sessionId":"0b22","message":{"role":"user","content":"Fix the parser."}}"#;
    let reply = r#"{"type":"assistant","isSidechain":false,"uuid":"a-1","sessionId":"0b22","message":{"id":"msg_1","model":"claude-sonnet-5-5","content":[{"type":"text","text":"Fixed the parser."}]}}"#;
    let later = r#"{"type":"user","isSidechain":false,"uuid":"u-2","sessionId":"0b22","message":{"role":"user","content":"and the docs?"}}"#;
    let run = |stem: &str, on_disk: &str| {
        std::fs::write(&transcript, format!("{prompt}\n")).expect("transcript");
        let snap = aterm_agent::harness::result::snapshot(
            &dir.path().join("tasks"),
            stem,
            Some((&transcript, &format!("{prompt}\n"))),
            aterm_agent::harness::result::DoneBy::Turn,
        )
        .expect("written");
        assert_eq!(snap.unavailable, Some("no-reply"));
        let done = TaskMark {
            why: Some("landed"),
            result_bytes: 0,
            result_path: Some(snap.path.clone()),
            ..TaskMark::new(TaskState::Done)
        };
        let (marked, _) = mark_session_task(ctx, 6, &done).expect("unfenced");
        assert_eq!(
            cmd_result(ctx, 6, ""),
            "ERR result unavailable why=no-reply\n",
            "nothing written yet"
        );
        std::fs::write(&transcript, on_disk).expect("transcript");
        (snap.path, marked.rev)
    };
    let rev_and_bytes = || {
        let timeline = ctx.timeline.lock().unwrap();
        let r = timeline.task().expect("a record");
        (r.rev, r.result_bytes)
    };
    let (path, rev) = run("s-6-1", &format!("{prompt}\n{reply}\n"));
    let answer = "OK 1 source=transcript session=0b22 uuid=a-1 bytes=17\nFixed the parser.\n";
    assert_eq!(cmd_result(ctx, 6, ""), answer);
    assert_eq!(rev_and_bytes(), (rev + 1, 17), "the record says it");
    assert_eq!(
        std::fs::read_to_string(&path).expect("snapshot"),
        "Fixed the parser."
    );
    assert_eq!(cmd_result(ctx, 6, ""), answer);
    assert_eq!(rev_and_bytes(), (rev + 1, 17), "read again no more");
    let (path, rev) = run(
        "s-6-2",
        &format!("{prompt}\n{later}\n{}\n", reply.replace("a-1", "a-2")),
    );
    assert_eq!(
        cmd_result(ctx, 6, ""),
        "ERR result unavailable why=no-reply\n"
    );
    assert_eq!(rev_and_bytes(), (rev, 0));
    assert_eq!(std::fs::read_to_string(&path).expect("snapshot"), "");
}

#[test]
fn a_turn_into_a_done_worker_opens_it_again_unless_the_harness_typed_it() {
    let handle = crate::session_store::test_handle(7);
    let ctx = &handle.ctx;
    let state = || {
        ctx.timeline
            .lock()
            .unwrap()
            .task()
            .map(|r| (r.rev, r.state, r.why))
    };
    let found = || ctx.timeline.lock().unwrap().task().map(|r| r.entered);
    reopen_at_turn(ctx, 7, &TurnBy::Ctl, found());
    assert_eq!(state(), None, "no task: none made");
    let done = |why| TaskMark {
        why: Some(why),
        ..TaskMark::new(TaskState::Done)
    };
    mark_session_task(ctx, 7, &done("landed")).expect("unfenced");
    reopen_at_turn(ctx, 7, &TurnBy::Harness, found());
    assert_eq!(state(), Some((1, TaskState::Done, Some("landed"))));
    reopen_at_turn(ctx, 7, &TurnBy::Ctl, found());
    assert_eq!(state(), Some((2, TaskState::Working, Some("turn"))));
    reopen_at_turn(ctx, 7, &TurnBy::Ctl, found());
    assert_eq!(state(), Some((2, TaskState::Working, Some("turn"))));
    for by in [
        TurnBy::Bridge,
        TurnBy::Sid(SID.to_string()),
        TurnBy::Unknown,
    ] {
        mark_session_task(ctx, 7, &done("done-check")).expect("unfenced");
        reopen_at_turn(ctx, 7, &by, found());
        assert_eq!(state().map(|s| s.1), Some(TaskState::Working), "{by:?}");
    }
    mark_session_task(ctx, 7, &done("landed")).expect("unfenced");
    let entered = found();
    let mut turn = TaskMark::new(TaskState::Working);
    turn.why = Some("turn");
    mark_session_task(ctx, 7, &turn).expect("unfenced");
    mark_session_task(ctx, 7, &done("landed")).expect("unfenced");
    let published = state();
    reopen_at_turn(ctx, 7, &TurnBy::Ctl, entered);
    assert_eq!(state(), published, "the done the Enter did not find stands");
    reopen_at_turn(ctx, 7, &TurnBy::Ctl, found());
    assert_eq!(state().map(|s| s.1), Some(TaskState::Working));
    mark_session_task(ctx, 7, &TaskMark::new(TaskState::Stalled)).expect("unfenced");
    let before = state();
    reopen_at_turn(ctx, 7, &TurnBy::Ctl, found());
    assert_eq!(state(), before, "only a done task is opened again");
}

#[test]
fn a_done_marked_again_between_the_enter_and_the_submit_still_opens() {
    let handle = crate::session_store::test_handle(7);
    let ctx = &handle.ctx;
    let record = || {
        ctx.timeline
            .lock()
            .unwrap()
            .task()
            .cloned()
            .expect("a task")
    };
    let done = TaskMark {
        why: Some("landed"),
        ..TaskMark::new(TaskState::Done)
    };
    mark_session_task(ctx, 7, &TaskMark::new(TaskState::Working)).expect("unfenced");
    mark_session_task(ctx, 7, &done).expect("unfenced");
    let at_enter = record();
    let n = at_enter.rev;
    let repaired = TaskMark {
        result_bytes: 42,
        ..done.clone()
    };
    let same = |now: Option<&TaskRecord>| now.is_some_and(|now| now.rev == n);
    let marked = mark_session_task_if(ctx, 7, &repaired, Duration::ZERO, same).expect("unfenced");
    assert_eq!(marked.map(|(r, moved)| (r.rev, moved)), Some((n + 1, true)));
    crate::session_timeline::write_attention_owned(
        ctx,
        crate::harness_host::ATTENTION_OWNER,
        crate::session_timeline::MetaEdit::Set("a box no rule answers"),
        aterm_slog::Actor::Harness,
    )
    .expect("written");
    let remarked = record();
    assert_eq!(
        (remarked.rev, remarked.state, remarked.entered),
        (n + 2, TaskState::Done, at_enter.entered),
        "the same done, two revs on"
    );
    assert!(remarked.rev > n && remarked.has_news());
    reopen_at_turn(ctx, 7, &TurnBy::Ctl, Some(at_enter.entered - 1));
    assert_eq!(
        record(),
        remarked,
        "an entry the Enter did not find opens nothing"
    );
    reopen_at_turn(ctx, 7, &TurnBy::Ctl, Some(at_enter.entered));
    let opened = record();
    assert_eq!(
        (opened.rev, opened.state, opened.why),
        (n + 3, TaskState::Working, Some("turn"))
    );
    crate::session_timeline::write_attention_owned(
        ctx,
        crate::harness_host::ATTENTION_OWNER,
        crate::session_timeline::MetaEdit::Clear,
        aterm_slog::Actor::Harness,
    )
    .expect("cleared");
    assert!(
        !record().has_news(),
        "working, nothing raised: no news past n"
    );
    mark_session_task(ctx, 7, &done).expect("unfenced");
    let own = record();
    assert!(own.rev > n && own.state == TaskState::Done && own.entered > at_enter.entered);
}

#[test]
fn a_conditional_mark_never_overwrites_a_state_that_landed_first() {
    let handle = crate::session_store::test_handle(7);
    let ctx = &handle.ctx;
    let is_done = |r: Option<&TaskRecord>| r.is_some_and(|r| r.state == TaskState::Done);
    let working = TaskMark {
        why: Some("turn"),
        ..TaskMark::new(TaskState::Working)
    };
    let exited = TaskMark {
        why: Some("exit"),
        ..TaskMark::new(TaskState::Exited)
    };
    mark_session_task(ctx, 7, &exited).expect("unfenced");
    let landed = mark_session_task_if(ctx, 7, &working, Duration::ZERO, is_done).expect("unfenced");
    assert_eq!(landed, None);
    let state = || {
        ctx.timeline
            .lock()
            .unwrap()
            .task()
            .map(|r| (r.rev, r.state))
    };
    assert_eq!(state(), Some((1, TaskState::Exited)), "the exit stands");
    mark_session_task(ctx, 7, &TaskMark::new(TaskState::Done)).expect("unfenced");
    let landed = mark_session_task_if(ctx, 7, &working, Duration::ZERO, is_done).expect("unfenced");
    assert_eq!(
        landed.map(|(r, changed)| (r.state, changed)),
        Some((TaskState::Working, true))
    );
    assert_eq!(state(), Some((3, TaskState::Working)));
}

#[test]
fn a_record_entering_a_state_rings_the_host_and_a_re_mark_does_not() {
    let handle = crate::session_store::test_handle(7);
    let ctx = &handle.ctx;
    let rings = crate::harness_host::rings_here;
    let rung = |mark: &TaskMark| {
        let before = rings();
        mark_session_task(ctx, 7, mark).expect("unfenced");
        rings() - before
    };
    let done = TaskMark {
        why: Some("landed"),
        ..TaskMark::new(TaskState::Done)
    };
    assert_eq!(rung(&TaskMark::new(TaskState::Working)), 1, "a new record");
    assert_eq!(rung(&done), 1, "done entered");
    let repaired = TaskMark {
        result_bytes: 42,
        ..done.clone()
    };
    assert_eq!(rung(&repaired), 0, "the same done, marked again");
    assert_eq!(rung(&repaired), 0, "nothing changed");
    let before = rings();
    let not_done = |r: Option<&TaskRecord>| r.is_some_and(|r| r.state != TaskState::Done);
    let reopened = TaskMark {
        why: Some("turn"),
        ..TaskMark::new(TaskState::Working)
    };
    let landed =
        mark_session_task_if(ctx, 7, &reopened, Duration::ZERO, not_done).expect("unfenced");
    assert_eq!(
        (landed, rings() - before),
        (None, 0),
        "a mark that did not land"
    );
    reopen_at_turn(ctx, 7, &TurnBy::Ctl, Some(2));
    assert_eq!(
        rings() - before,
        1,
        "{:?}",
        ctx.timeline.lock().unwrap().task()
    );
}

#[test]
fn a_moving_mark_posts_one_task_wake_and_a_still_or_fenced_one_none() {
    let handle = crate::session_store::test_handle(9);
    let ctx = &handle.ctx;
    let _ = crate::presence::take_task_wakes();
    let mark = |m: &TaskMark| {
        mark_session_task(ctx, 9, m).expect("unfenced");
        crate::presence::take_task_wakes()
    };
    let stalled = TaskMark {
        why: Some("startup"),
        ..TaskMark::new(TaskState::Stalled)
    };
    assert_eq!(
        mark(&TaskMark::new(TaskState::Working)),
        [9],
        "a new record"
    );
    assert_eq!(mark(&stalled), [9], "a stall");
    assert_eq!(mark(&stalled), Vec::<u64>::new(), "the same mark again");
    let waiter = crate::session_watch::Subscription::single(&ctx.watchers);
    {
        let mut g = ctx.timeline.lock().unwrap();
        let at = g.slog().now();
        g.slog_mut()
            .observe_story("chose", aterm_slog::Actor::Ctl, at);
    }
    assert_eq!(
        mark(&stalled),
        Vec::<u64>::new(),
        "a log wake alone posts no need verdict"
    );
    assert!(
        waiter.wait(Duration::ZERO),
        "the log's wake woke the session's waiters"
    );
    drop(waiter);
    let raised = mark_session_task_with(
        ctx,
        9,
        None,
        AttentionEdit::Raise("the worker never started: a login"),
        Duration::ZERO,
        |_, _| true,
    )
    .expect("unfenced")
    .expect("it held");
    assert!(raised.attention_moved);
    assert_eq!(crate::presence::take_task_wakes(), [9]);
    let landed = mark_session_task_if(
        ctx,
        9,
        &TaskMark::new(TaskState::Working),
        Duration::ZERO,
        |_| false,
    )
    .expect("unfenced");
    assert_eq!(landed, None);
    assert_eq!(crate::presence::take_task_wakes(), Vec::<u64>::new());
    let fenced = crate::session_store::test_handle(10);
    let _ = fenced.ctx.fabric.fence_hold(false);
    assert!(
        mark_session_task_within(
            &fenced.ctx,
            10,
            &TaskMark::new(TaskState::Working),
            Duration::ZERO,
        )
        .is_err()
    );
    assert_eq!(crate::presence::take_task_wakes(), Vec::<u64>::new());
}

fn hold_room(n: usize) -> Option<FactWord> {
    use aterm_agent::supervise::occupancy::RuleId;
    Some(FactWord::new(RuleId::HoldRoom, n).expect("a nonzero ordinal"))
}

/// `task`, an `await task` row and the `EVENT … task` row read `fact=<word|-> fact_rev=<n>`
/// once the word was first edited, and nothing before; the edit posts one task wake, and its
/// outcome names what it did.
#[test]
fn the_fact_word_round_trips_on_task_await_and_its_event() {
    let handle = crate::session_store::test_handle(11);
    let ctx = &handle.ctx;
    let fence = Duration::ZERO;
    assert_eq!(
        mark_session_task_fact(ctx, 11, hold_room(1), fence),
        NoteOutcome::NoTask
    );
    mark_session_task(ctx, 11, &TaskMark::new(TaskState::Working)).expect("unfenced");
    let reply = || cmd_task(ctx, 11, Scope::Owner, "");
    let events = || -> Vec<String> {
        ctx.timeline
            .lock()
            .unwrap()
            .since(None)
            .filter(|e| e.kind == "task-change")
            .map(|e| e.payload.to_string())
            .collect()
    };
    let record = || {
        ctx.timeline
            .lock()
            .unwrap()
            .task()
            .cloned()
            .expect("a task")
    };
    assert!(!reply().contains("fact"), "{}", reply());
    let _ = crate::presence::take_task_wakes();
    assert_eq!(
        mark_session_task_fact(ctx, 11, hold_room(1), fence),
        NoteOutcome::Noted
    );
    assert_eq!(crate::presence::take_task_wakes(), [11]);
    assert!(
        reply().ends_with(" fact=hold-room@v1#1 fact_rev=1 agent=- program_state=- attention=-\n"),
        "{}",
        reply()
    );
    assert_eq!(
        await_row(SID, Some(&record()), "-", "-", "-", false),
        format!(
            "@{SID} rev=1 state=working agent=- program_state=- attention=- result_bytes=0 \
             fact=hold-room@v1#1 fact_rev=1"
        )
    );
    assert_eq!(
        mark_session_task_fact(ctx, 11, hold_room(1), fence),
        NoteOutcome::Noted
    );
    assert_eq!(
        crate::presence::take_task_wakes(),
        Vec::<u64>::new(),
        "the word standing again moves nothing"
    );
    assert_eq!(
        mark_session_task_fact(ctx, 11, None, fence),
        NoteOutcome::Cleared
    );
    assert_eq!(
        mark_session_task_fact(ctx, 11, None, fence),
        NoteOutcome::Cleared
    );
    assert!(reply().contains(" fact=- fact_rev=2 "), "{}", reply());
    assert_eq!(
        events(),
        [
            "rev=1 state=working attention=0",
            "rev=1 state=working attention=0 fact=hold-room@v1#1 fact_rev=1",
            "rev=1 state=working attention=0 fact=- fact_rev=2",
        ]
    );
    mark_session_task(ctx, 11, &TaskMark::new(TaskState::Exited)).expect("unfenced");
    assert_eq!(
        mark_session_task_fact(ctx, 11, hold_room(2), fence),
        NoteOutcome::Closed,
        "an exited record takes no word"
    );
    assert_eq!(
        mark_session_task_fact(ctx, 11, None, fence),
        NoteOutcome::Cleared
    );
    let fenced = crate::session_store::test_handle(12);
    mark_session_task(&fenced.ctx, 12, &TaskMark::new(TaskState::Working)).expect("unfenced");
    let _ = crate::presence::take_task_wakes();
    let _ = fenced.ctx.fabric.fence_hold(false);
    assert_eq!(
        mark_session_task_fact(&fenced.ctx, 12, hold_room(1), fence),
        NoteOutcome::Fenced
    );
    assert_eq!(crate::presence::take_task_wakes(), Vec::<u64>::new());
    assert_eq!(
        fenced
            .ctx
            .timeline
            .lock()
            .unwrap()
            .task()
            .map(|t| t.fact_rev),
        Some(0)
    );
}

/// The carry row holds the fact word and its `fact_rev=`, so a successor's note of the word it
/// finds standing moves nothing. A missing or malformed fact key, or a word at rev 0, reads as
/// no word or rev 0 rather than refusing the record; a key carried twice is refused.
#[test]
fn the_carry_holds_the_fact_word_and_its_rev_and_reads_them_leniently() {
    let t0 = Instant::now();
    let record = TaskRecord {
        rev: 4,
        state: TaskState::Working,
        from: "ctl".into(),
        manager_node: None,
        told: Default::default(),
        why: None,
        checked: None,
        left: None,
        exit: None,
        finished: false,
        unfinished_exit: false,
        result_bytes: 0,
        result_path: None,
        attention: false,
        asks: Asks::default(),
        since_unix_ms: 10_000,
        since: t0,
        started_unix_ms: 10_000,
        started: t0,
        entered: 1,
        carried_entered: None,
        agent: None,
        program: None,
        trusted_cwd: None,
        fact: hold_room(2),
        program_block: None,
        fact_rev: 3,
    };
    let row = record.carry_row(t0);
    assert_eq!(
        row,
        "rev=4 state=working from=ctl since=10000 result_bytes=0 attention=0 age_ms=0 \
         fact=hold-room@v1#2 fact_rev=3"
    );
    let back = TaskRecord::from_carry_row(&row, None, t0, 10_000).expect("reads");
    assert_eq!((back.fact, back.fact_rev), (hold_room(2), 3));
    assert_eq!(back.carry_row(t0), row);
    assert_eq!(
        fact_edit(&back, hold_room(2)),
        (NoteOutcome::Noted, None),
        "the successor's note of the word it finds standing moves nothing"
    );
    let dropped = TaskRecord {
        fact: None,
        ..back.clone()
    };
    assert_eq!(
        fact_edit(&dropped, hold_room(2)).1.map(|r| r.fact_rev),
        Some(4),
        "CONTROL: a word the carry dropped is noted as a new edit"
    );
    let cleared = TaskRecord {
        fact: None,
        ..record.clone()
    };
    let row = cleared.carry_row(t0);
    assert!(row.ends_with(" attention=0 age_ms=0 fact_rev=3"), "{row}");
    let back = TaskRecord::from_carry_row(&row, None, t0, 10_000).expect("reads");
    assert_eq!((back.fact, back.fact_rev), (None, 3));
    let older = row.replace(" fact_rev=3", "");
    let bare = TaskRecord::from_carry_row(&older, None, t0, 10_000).expect("an older row reads");
    assert_eq!((bare.fact, bare.fact_rev), (None, 0));
    assert_eq!(
        bare.carry_row(t0),
        older,
        "an unedited word carries nothing"
    );
    for odd in ["fact_rev=x", "fact_rev=-1", "fact_rev="] {
        let row = format!("{older} {odd}");
        let read = TaskRecord::from_carry_row(&row, None, t0, 10_000).expect("still reads");
        assert_eq!(read.fact_rev, 0, "{odd}");
    }
    for word in [
        "ignore.previous.instructions",
        "hold-room@v1#0",
        "hold-room@v9#1",
        "-",
        "",
    ] {
        let row = format!("{older} fact={word} fact_rev=2");
        let read = TaskRecord::from_carry_row(&row, None, t0, 10_000).expect("still reads");
        assert_eq!((read.fact, read.fact_rev), (None, 2), "{word:?}");
    }
    let unrevised = format!("{older} fact=hold-room@v1#9");
    let read = TaskRecord::from_carry_row(&unrevised, None, t0, 10_000).expect("reads");
    assert_eq!(
        (read.fact, read.fact_rev),
        (None, 0),
        "a word with no rev past 0 stands for no edit"
    );
    let twice = format!("{} fact_rev=4", record.carry_row(t0));
    assert_eq!(TaskRecord::from_carry_row(&twice, None, t0, 10_000), None);
    let twice = format!("{} fact=hold-room@v1#1", record.carry_row(t0));
    assert_eq!(TaskRecord::from_carry_row(&twice, None, t0, 10_000), None);
}

/// Every edit a record took before the fact word leaves the word and its rev as they stand,
/// and the fact edit leaves `rev`, `entered`, `since` and `state` as they stand.
#[test]
fn no_mark_moves_the_fact_and_the_fact_edit_moves_nothing_else() {
    use crate::session_timeline::{MetaEdit, write_attention_owned};
    let handle = crate::session_store::test_handle(13);
    let ctx = &handle.ctx;
    let fence = Duration::ZERO;
    mark_session_task(ctx, 13, &TaskMark::new(TaskState::Working)).expect("unfenced");
    assert_eq!(
        mark_session_task_fact(ctx, 13, hold_room(1), fence),
        NoteOutcome::Noted
    );
    let record = || {
        ctx.timeline
            .lock()
            .unwrap()
            .task()
            .cloned()
            .expect("a task")
    };
    let fact = || {
        let r = record();
        (r.fact, r.fact_rev)
    };
    let standing = (hold_room(1), 1);
    type Edit<'a> = (&'a str, Box<dyn Fn() + 'a>);
    let edits: Vec<Edit<'_>> = vec![
        (
            "a mark into a new state",
            Box::new(|| {
                mark_session_task(ctx, 13, &TaskMark::new(TaskState::Stalled)).expect("unfenced");
            }),
        ),
        (
            "a mark in the same state with new facts",
            Box::new(|| {
                let mark = TaskMark {
                    why: Some("held"),
                    result_bytes: 4,
                    ..TaskMark::new(TaskState::Stalled)
                };
                mark_session_task(ctx, 13, &mark).expect("unfenced");
            }),
        ),
        (
            "the supervisor's attention raised",
            Box::new(|| {
                write_attention_owned(
                    ctx,
                    crate::harness_host::ATTENTION_OWNER,
                    MetaEdit::Set("a login"),
                    aterm_slog::Actor::Harness,
                )
                .unwrap();
            }),
        ),
        (
            "a mark that takes the attention down",
            Box::new(|| {
                let _ = cmd_task(ctx, 13, Scope::Owner, "mark done why=landed");
            }),
        ),
        (
            "a turn opening the done task again",
            Box::new(|| {
                let found = ctx.timeline.lock().unwrap().task().map(|r| r.entered);
                reopen_at_turn(ctx, 13, &TurnBy::Ctl, found);
            }),
        ),
        (
            "a spawn naming its worker",
            Box::new(|| {
                ctx.timeline
                    .lock()
                    .unwrap()
                    .set_task_worker(aterm_phase::Program::Claude, None);
            }),
        ),
        (
            "an exit",
            Box::new(|| {
                let mark = TaskMark {
                    exit: Some(0),
                    ..TaskMark::new(TaskState::Exited)
                };
                mark_session_task(ctx, 13, &mark).expect("unfenced");
            }),
        ),
    ];
    for (edit, apply) in &edits {
        let before = record();
        apply();
        assert_ne!(record(), before, "{edit}: the edit took");
        assert_eq!(fact(), standing, "{edit}");
    }
    mark_session_task(ctx, 13, &TaskMark::new(TaskState::Working)).expect("unfenced");
    let before = record();
    assert_eq!(
        mark_session_task_fact(ctx, 13, hold_room(2), fence),
        NoteOutcome::Noted
    );
    let after = record();
    assert_eq!(
        (
            after.rev,
            after.entered,
            after.since,
            after.since_unix_ms,
            after.state
        ),
        (
            before.rev,
            before.entered,
            before.since,
            before.since_unix_ms,
            before.state
        )
    );
    assert_eq!((after.fact, after.fact_rev), (hold_room(2), 2));
    assert_eq!(
        TaskRecord {
            fact: before.fact,
            fact_rev: before.fact_rev,
            ..after
        },
        before,
        "the word and its rev, and nothing else"
    );
}

/// `result`'s repair CAS compares the `rev` it read, and a fact edit between the read and the
/// repair moves no `rev`, so the repair still lands. The negative control: under
/// `NoteBumpsNews` (the edit moves `rev`) the CAS misses and the repair is lost.
#[test]
fn the_repair_cas_still_matches_after_a_fact_edit() {
    use crate::occupancy_host::fault::{Fault, with};
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-task-fact-repair-")
        .tempdir()
        .expect("scratch");
    let path = dir.path().join("s-14-1.result.md");
    let found = aterm_agent::harness::result::Reread {
        report: aterm_agent::harness::result::Report {
            text: "Fixed the parser.".to_string(),
            session: "0b22".to_string(),
            uuid: "a-1".to_string(),
        },
        transcript: dir.path().join("t.jsonl"),
        from: aterm_agent::harness::result::Conversation::Transcript,
    };
    let repaired = |bump: bool| {
        let handle = crate::session_store::test_handle(14);
        let ctx = &handle.ctx;
        let done = TaskMark {
            result_path: Some(path.clone()),
            ..TaskMark::new(TaskState::Done)
        };
        mark_session_task(ctx, 14, &done).expect("unfenced");
        let read = ctx
            .timeline
            .lock()
            .unwrap()
            .task()
            .cloned()
            .expect("a task");
        let note = || mark_session_task_fact(ctx, 14, hold_room(1), Duration::ZERO);
        let noted = if bump {
            with(Fault::NoteBumpsNews, note)
        } else {
            note()
        };
        assert_eq!(noted, NoteOutcome::Noted);
        repair_result(ctx, 14, &read, &path, &found);
        let now = ctx
            .timeline
            .lock()
            .unwrap()
            .task()
            .cloned()
            .expect("a task");
        (now.result_bytes, now.fact)
    };
    assert_eq!(repaired(false), (found.bytes(), hold_room(1)));
    assert_eq!(
        repaired(true),
        (0, hold_room(1)),
        "CONTROL: a fact edit that moves the rev loses the repair"
    );
}

/// What a worker can write onto its own session (its attention text, title, description and
/// role) never reaches `fact=`: before any fact edit no `fact=` token stands on `task` or an
/// `await task` row, and after one exactly one stands, the closed word noted.
#[test]
fn a_planted_attention_or_title_never_reaches_fact() {
    use crate::session_timeline::{MetaEdit, write_attention_owned};
    let handle = crate::session_store::test_handle(15);
    let ctx = &handle.ctx;
    mark_session_task(ctx, 15, &TaskMark::new(TaskState::Working)).expect("unfenced");
    let plant = "x fact=wake@v1#7 fact_rev=99";
    write_attention_owned(
        ctx,
        crate::harness_host::NEEDS_ATTENTION_OWNER,
        MetaEdit::Set(plant),
        aterm_slog::Actor::Ctl,
    )
    .unwrap();
    {
        let mut meta = ctx.meta.lock().unwrap();
        meta.user_title = Some(plant.to_string());
        meta.description = Some(plant.to_string());
        meta.role = Some(plant.to_string());
    }
    let facts = |line: &str| -> Vec<String> {
        line.split_whitespace()
            .filter(|t| t.starts_with("fact=") || t.starts_with("fact_rev="))
            .map(str::to_string)
            .collect()
    };
    let rows = || {
        let reading = task_reading(ctx);
        [
            cmd_task(ctx, 15, Scope::Owner, ""),
            await_row(
                SID,
                reading.record.as_ref(),
                reading.agent,
                &reading.program,
                &reading.attention,
                false,
            ),
        ]
    };
    for row in rows() {
        assert!(
            row.contains("attention=x%20fact"),
            "the plant stands: {row}"
        );
        assert_eq!(facts(&row), Vec::<String>::new(), "{row}");
    }
    assert_eq!(
        mark_session_task_fact(ctx, 15, hold_room(1), Duration::ZERO),
        NoteOutcome::Noted
    );
    for row in rows() {
        assert_eq!(facts(&row), ["fact=hold-room@v1#1", "fact_rev=1"], "{row}");
    }
}

/// A write that passed the worker's Prompt gate may wait behind an update fence.
/// Its live authority must be read again after the wait, for notes and clears alike.
#[cfg(unix)]
fn task_fact_after_fenced_mode_change(prompt: bool, clear: bool) {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let handle = crate::session_store::test_handle(331);
    let ctx = Arc::clone(&handle.ctx);
    mark_session_task(&ctx, 331, &TaskMark::new(TaskState::Working)).expect("unfenced");
    if clear {
        assert_eq!(
            mark_session_task_fact(&ctx, 331, hold_room(1), Duration::ZERO),
            NoteOutcome::Noted
        );
    }
    let before = ctx.timeline.lock().unwrap().task().cloned().unwrap();
    let event_count = ctx.timeline.lock().unwrap().since(None).count();
    let _ = crate::presence::take_task_wakes();
    ctx.fabric.fence_hold(true);
    let blocked = ctx.fabric.fence_waits();
    let mode = Arc::new(AtomicBool::new(true));
    let writer = {
        let ctx = Arc::clone(&ctx);
        let mode = Arc::clone(&mode);
        std::thread::spawn(move || {
            let word = if clear { None } else { hold_room(1) };
            let outcome =
                mark_session_task_fact_if(&ctx, 331, word, aterm_spec::wait::MUST_HAPPEN, || {
                    mode.load(Ordering::Acquire)
                });
            (outcome, crate::presence::take_task_wakes())
        })
    };
    crate::fabric::assert_parked_on_the_fence(
        &ctx.fabric,
        blocked,
        || writer.is_finished(),
        "the task fact write waits on the update fence",
    );
    mode.store(prompt, Ordering::Release);
    ctx.fabric.lift_hold_fence();
    let (outcome, wakes) = writer.join().unwrap();
    let timeline = ctx.timeline.lock().unwrap();
    let after = timeline.task().cloned().unwrap();
    if prompt {
        assert_eq!(
            outcome,
            if clear {
                NoteOutcome::Cleared
            } else {
                NoteOutcome::Noted
            }
        );
        assert_eq!(wakes, [331]);
        assert_eq!(after.fact, if clear { None } else { hold_room(1) });
        assert_eq!(after.fact_rev, before.fact_rev + 1);
        assert_eq!(after.rev, before.rev);
        assert_eq!(timeline.since(None).count(), event_count + 1);
    } else {
        assert_eq!(outcome, NoteOutcome::Fenced, "clear={clear}");
        assert!(wakes.is_empty(), "a withdrawn write posts no task wake");
        assert_eq!(after, before, "the fact and both revisions stay unchanged");
        assert_eq!(timeline.since(None).count(), event_count);
    }
}

#[cfg(unix)]
#[test]
fn a_task_fact_write_refuses_mode_withdrawn_while_fenced() {
    task_fact_after_fenced_mode_change(false, false);
}

#[cfg(unix)]
#[test]
fn a_task_fact_clear_refuses_mode_withdrawn_while_fenced() {
    task_fact_after_fenced_mode_change(false, true);
}

#[cfg(unix)]
#[test]
fn a_task_fact_write_keeps_live_prompt_authority_after_the_fence() {
    task_fact_after_fenced_mode_change(true, false);
    task_fact_after_fenced_mode_change(true, true);
}

/// A DONE MARKS WHAT ATERM CHECKED (§14.9, slice 9c): `task mark done checked=<word>` keeps the
/// word on the record, which `task` (its wire fields), a seamless update's carry row and an
/// `await task` row say after `why=`, and a carry row reads it back; the next mark (someone
/// else's turn reopening it) clears it. Each receipt's word is taken (`<head>@<ref>` and every
/// other leg's), and nothing else: not on a mark that is not done, not a head that is no 12
/// lowercase digits, not the on-main leg's bare word; a carry row holding a word no mark takes is
/// no row. NEGATIVE CONTROL (run by hand): `push_optional` without the word fails the round trip.
#[test]
fn a_done_marks_what_aterm_checked_and_the_record_carries_it() {
    let t0 = Instant::now();
    let on = "3f9a1c2e4b7d@origin/main";
    let mark = parse(&format!("done why=landed checked={on} result_bytes=12")).expect("a mark");
    assert_eq!(mark.checked.as_deref(), Some(on));
    let working = TaskMark::new(TaskState::Working)
        .apply(None, &Asks::default(), t0, 9_000)
        .0;
    let (record, moved) = mark.apply(Some(&working), &Asks::default(), t0, 10_000);
    assert!(moved);
    assert_eq!(record.checked.as_deref(), Some(on));
    let wire = record.wire_fields();
    assert!(
        wire.contains(&format!(" why=landed checked={on}")),
        "{wire}"
    );
    let row = record.carry_row(t0);
    assert!(row.contains(&format!(" why=landed checked={on}")), "{row}");
    let back = TaskRecord::from_carry_row(&row, Some(&tasks()), t0, 10_000).expect("reads");
    assert_eq!(back.carry_row(t0), row);
    assert_eq!(back.checked.as_deref(), Some(on));
    assert!(
        await_row(SID, Some(&record), "idle", "-", "-", false).contains(&format!(" checked={on}")),
        "{}",
        await_row(SID, Some(&record), "idle", "-", "-", false)
    );
    let (again, _) = mark.apply(Some(&record), &Asks::default(), t0, 11_000);
    assert_eq!(again.rev, record.rev, "the same mark again moves nothing");
    let (reopened, _) = parse("working why=turn").expect("a reopening").apply(
        Some(&record),
        &Asks::default(),
        t0,
        12_000,
    );
    assert_eq!(reopened.checked, None, "{reopened:?}");
    assert!(!reopened.wire_fields().contains("checked="));
    for word in [
        "pushed",
        "no-remote",
        "no-repo",
        "gone",
        "0c4d2e9f1a7b@up%20stream/main",
    ] {
        assert_eq!(
            parse(&format!("done checked={word}")).map(|m| m.checked),
            Ok(Some(word.to_string())),
            "{word}"
        );
    }
    for bad in [
        "on-main",
        "landed",
        "3F9A1C2E4B7D@origin/main",
        "3f9a1c2e4b7@origin/main",
        "3f9a1c2e4b7d@",
        "3f9a1c2e4b7dorigin/main",
    ] {
        assert!(parse(&format!("done checked={bad}")).is_err(), "{bad}");
        assert_eq!(
            TaskRecord::from_carry_row(&row.replace(on, bad), Some(&tasks()), t0, 10_000),
            None,
            "{bad}"
        );
    }
    assert_eq!(
        parse("working checked=no-repo").map(|m| m.checked),
        Err("ERR task mark: checked= marks a done task\n".to_string())
    );
}

/// A LANDING'S MARK KEEPS ITS LEFT WORD (`docs/DESIGN-leftovers-by-default-2026-10-30.md` §2):
/// `task mark done left=<word>` keeps what the landing read of the agent's left processes on the
/// record, which `task` (its wire fields), a carry row and an `await task` row say after
/// `checked=`, and a carry row reads it back; the next mark clears it. Each word the done line
/// writes is taken (`0`, `kept:<n>`, `<n>`, `unchecked`) and nothing else, nor on a mark that is
/// not done, nor from a hand. NEGATIVE CONTROL: a word taken unread takes the malformed ones.
#[test]
fn a_landing_mark_keeps_its_left_word() {
    use crate::control::{Principal, ServingPrincipal};
    let t0 = Instant::now();
    let mark = parse("done why=landed left=kept:2 result_bytes=12").expect("a mark");
    assert_eq!(mark.left.as_deref(), Some("kept:2"));
    let working = TaskMark::new(TaskState::Working)
        .apply(None, &Asks::default(), t0, 9_000)
        .0;
    let (record, moved) = mark.apply(Some(&working), &Asks::default(), t0, 10_000);
    assert!(moved);
    assert_eq!(record.left.as_deref(), Some("kept:2"));
    assert!(
        record.wire_fields().contains(" why=landed left=kept:2"),
        "{}",
        record.wire_fields()
    );
    let row = record.carry_row(t0);
    let back = TaskRecord::from_carry_row(&row, Some(&tasks()), t0, 10_000).expect("reads");
    assert_eq!(back.carry_row(t0), row);
    assert_eq!(back.left.as_deref(), Some("kept:2"));
    let waited = await_row(SID, Some(&record), "idle", "-", "-", false);
    assert!(waited.contains(" left=kept:2"), "{waited}");
    let (reopened, _) = parse("working why=turn").expect("a reopening").apply(
        Some(&record),
        &Asks::default(),
        t0,
        12_000,
    );
    assert_eq!(reopened.left, None, "{reopened:?}");
    for word in ["0", "kept:1", "3", "unchecked"] {
        assert_eq!(
            parse(&format!("done left={word}")).map(|m| m.left),
            Ok(Some(word.to_string())),
            "{word}"
        );
    }
    for bad in [
        "",
        "-1",
        "01",
        "kept:0",
        "kept:",
        "some",
        "unchecked:1",
        "65536",
    ] {
        assert_eq!(
            parse(&format!("done left={bad}")).map(|m| m.left),
            Err("ERR task mark: left= is 0, kept:<n>, <n> or unchecked\n".to_string()),
            "{bad:?}"
        );
        assert_eq!(
            TaskRecord::from_carry_row(
                &row.replace("left=kept:2", &format!("left={bad}")),
                Some(&tasks()),
                t0,
                10_000
            ),
            None,
            "{bad:?}"
        );
    }
    assert_eq!(
        parse("working left=0").map(|m| m.left),
        Err("ERR task mark: left= marks a done task\n".to_string())
    );
    let handle = crate::session_store::test_handle(7);
    let ctx = &handle.ctx;
    {
        let _hand = ServingPrincipal::enter(Principal::Instance);
        assert_eq!(
            cmd_task(ctx, 7, Scope::Owner, "mark done why=landed left=0"),
            LEFT_BY_HAND
        );
    }
    let _harness = ServingPrincipal::enter(Principal::Harness);
    let marked = cmd_task(ctx, 7, Scope::Owner, "mark done why=landed left=0");
    assert!(marked.contains(" left=0"), "{marked}");
}

#[test]
fn unicode_receipts_mark_done_and_roundtrip_through_records_and_result_sidecars() {
    use aterm_agent::harness::acts::{LandReceipt, ReceiptLeg};
    use aterm_agent::harness::land_receipt::{CHECKED_WORD_BYTES, HEAD_DIGITS};
    use aterm_agent::harness::result::{self, DoneBy};

    let scratch = aterm_tempfile::tempdir().expect("private data-only result fixture");
    let now = Instant::now();
    for (index, reference) in [
        format!("origin/{}", "日本語".repeat(5)),
        format!("origin/{}/{}", "🚀".repeat(32), "🚀".repeat(32)),
    ]
    .into_iter()
    .enumerate()
    {
        let receipt = LandReceipt {
            head: "3f9a1c2e4b7d".to_string(),
            main_ref: reference.clone(),
            leg: ReceiptLeg::OnMain,
            ..LandReceipt::default()
        };
        let word = receipt.checked_word();
        assert!(word.len() > 128 && word.len() <= CHECKED_WORD_BYTES);
        assert_eq!(
            aterm_codec::pct::decode(word.split_once('@').unwrap().1),
            reference
        );
        let snapshot = result::snapshot_screen(
            scratch.path(),
            &format!("s-ab12-{}", index + 1),
            "claude",
            "Nothing left: the Unicode branch is landed.",
            DoneBy::Turn,
        )
        .expect("snapshot");
        let mark = TaskMark::parse(
            &format!(
                "done why=landed checked={word} result_bytes={} result_path={}",
                snapshot.bytes,
                aterm_codec::pct::encode(&snapshot.path.to_string_lossy()),
            ),
            Some(scratch.path()),
        )
        .expect("the receipt's token is a valid done mark");
        let working = TaskMark::new(TaskState::Working)
            .apply(None, &Asks::default(), now, 9_000)
            .0;
        let (record, changed) = mark.apply(Some(&working), &Asks::default(), now, 10_000);
        assert!(changed && record.state == TaskState::Done);
        assert_eq!(record.checked.as_deref(), Some(word.as_str()));
        let row = record.carry_row(now);
        let restored = TaskRecord::from_carry_row(&row, Some(scratch.path()), now, 10_000)
            .expect("the same Unicode token survives record restoration");
        assert_eq!(restored.checked, record.checked);
        assert!(
            await_row(SID, Some(&record), "idle", "-", "-", false)
                .contains(&format!(" checked={word}"))
        );

        result::note_checked(&snapshot.path, &word).expect("the sidecar takes the same token");
        assert_eq!(
            result::checked_of(&snapshot.path).as_deref(),
            Some(word.as_str())
        );
        let answer = result::answer(&snapshot.path, false);
        assert!(
            answer
                .lines()
                .next()
                .is_some_and(|header| header.starts_with("OK ")
                    && header.ends_with(&format!(" checked={word}"))),
            "{answer}"
        );
        let sidecar = std::fs::read(result::sidecar_of(&snapshot.path)).expect("sidecar");
        for invalid in [
            "3f9a1c2e4b7d@%".to_string(),
            "3f9a1c2e4b7d@%FF".to_string(),
            "3f9a1c2e4b7d@%00".to_string(),
            format!(
                "3f9a1c2e4b7d@{}",
                "x".repeat(CHECKED_WORD_BYTES - HEAD_DIGITS)
            ),
        ] {
            assert!(
                TaskMark::parse(&format!("done checked={invalid}"), Some(scratch.path())).is_err()
            );
            assert!(
                TaskRecord::from_carry_row(
                    &row.replace(&word, &invalid),
                    Some(scratch.path()),
                    now,
                    10_000
                )
                .is_none()
            );
            assert!(result::note_checked(&snapshot.path, &invalid).is_err());
        }
        assert_eq!(
            std::fs::read(result::sidecar_of(&snapshot.path)).unwrap(),
            sidecar
        );
    }
}

/// A CONCLUSION STANDS EXACTLY WHERE A TASK IS TERMINAL, over every state and every `why=`, so what
/// wakes a waiter is what can be read; and each state reads as the record says it ended.
#[test]
fn a_conclusion_stands_exactly_where_a_task_is_terminal() {
    for state in TaskState::ALL {
        for why in &[false, true] {
            assert_eq!(
                state.is_terminal(),
                state.conclusion(*why).is_some(),
                "{state:?} finished={why}"
            );
        }
    }
    for (state, why, conclusion) in [
        (TaskState::Working, None, None),
        (TaskState::Done, Some("landed"), Some(Conclusion::Finished)),
        (TaskState::Done, None, Some(Conclusion::Finished)),
        (TaskState::Exited, Some("exit"), Some(Conclusion::Failed)),
        (TaskState::Exited, None, Some(Conclusion::Failed)),
        (
            TaskState::Stalled,
            Some("startup"),
            Some(Conclusion::Failed),
        ),
        (
            TaskState::Failed,
            Some(WHY_COULD_NOT_FINISH),
            Some(Conclusion::Failed),
        ),
    ] {
        let record = TaskMark {
            why,
            ..TaskMark::new(state)
        }
        .apply(None, &Asks::default(), Instant::now(), 1_000)
        .0;
        assert_eq!(record.conclusion(), conclusion, "{state:?} why={why:?}");
    }
    assert_eq!(Conclusion::Finished.word(), "finished");
    assert_eq!(Conclusion::Failed.word(), "failed");
}

/// `conclusion=` AND `why=` ARE ON THE STATUS FIELDS AND THE `await task` ROW once the task ended,
/// and on no carry row, which holds what it is worked out from. A tab closed on a task with no
/// conclusion reads failed. NEGATIVE CONTROL: a working record prints none.
#[test]
fn the_wire_says_how_a_task_ended() {
    let t0 = Instant::now();
    let working = TaskMark::new(TaskState::Working)
        .apply(None, &Asks::default(), t0, 1_000)
        .0;
    assert!(!working.wire_fields().contains("conclusion="));
    let row = |record: &TaskRecord, closed: bool| {
        await_row(SID, Some(record), "claude", "-", "-", closed)
    };
    assert!(!row(&working, false).contains("conclusion="));
    assert!(row(&working, true).contains(" conclusion=failed"), "closed");

    let failed = TaskMark {
        why: Some(WHY_COULD_NOT_FINISH),
        ..TaskMark::new(TaskState::Failed)
    }
    .apply(Some(&working), &Asks::default(), t0, 2_000)
    .0;
    let fields = failed.wire_fields();
    assert!(fields.contains("state=failed"), "{fields}");
    assert!(fields.contains(" conclusion=failed"), "{fields}");
    assert!(fields.contains(" why=could-not-finish"), "{fields}");
    let row_words = row(&failed, false);
    assert!(row_words.contains(" conclusion=failed"), "{row_words}");
    assert!(row_words.contains(" why=could-not-finish"), "{row_words}");
    let carried = failed.carry_row(t0);
    assert!(!carried.contains("conclusion="), "{carried}");
    assert!(carried.contains("failed"), "{carried}");

    let landed = TaskMark {
        why: Some(WHY_LANDED),
        ..TaskMark::new(TaskState::Done)
    }
    .apply(Some(&working), &Asks::default(), t0, 2_000)
    .0;
    assert!(row(&landed, false).contains(" conclusion=finished"));
    assert!(
        row(&landed, true).contains(" conclusion=finished"),
        "closed after"
    );
    assert_eq!(
        parse(" failed why=could-not-finish").map(|m| m.state),
        Ok(TaskState::Failed)
    );
    for word in [
        WHY_COULD_NOT_FINISH,
        WHY_OFF,
        WHY_OFF_CTL,
        WHY_EXPIRED,
        WHY_REPEATED,
        WHY_EXIT,
    ] {
        assert!(TASK_WHY.contains(&word), "{word}");
        assert!(parse(&format!(" failed why={word}")).is_ok(), "{word}");
    }
}

/// EVERY ENDING AUTOPILOT LAND MARKS IS A WORD THE RECORD TAKES: aterm-agent's supervisor marks a
/// managed worker's task `failed why=<word>` for each of its endings (`GaveUp::ALL`, the usage
/// limit's among them), so each parses as a mark, is a closed `why=` word, and has a person's
/// words of its own; a word this record refused would leave the record `working` while the loop
/// types nothing more. NEGATIVE CONTROL: a word no ending names is refused.
#[test]
fn every_ending_the_supervisor_marks_is_a_word_the_record_takes() {
    use aterm_agent::supervise::policy::turn_end::GaveUp;
    for ending in GaveUp::ALL {
        let word = ending.word();
        assert!(TASK_WHY.contains(&word), "{word}");
        let mark = parse(&format!(" failed why={word}")).expect(word);
        assert_eq!((mark.state, mark.why), (TaskState::Failed, Some(word)));
        let said = failed_words(TaskState::Failed, Some(word));
        assert!(!said.starts_with("ended unfinished"), "{word}: {said}");
    }
    assert_eq!(WHY_USAGE_LIMIT, GaveUp::UsageLimit.word());
    assert!(parse(" failed why=usage-limits").is_err(), "CONTROL");
}

/// AN EXIT DOES NOT REWRITE HOW THE TASK ENDED: a landing's `why=` and `checked=` and a failure's
/// `why=` outlive it, and a look at them stays a look. NEGATIVE CONTROL: an agent that exits while
/// its task works reads failed, its own `why=exit` kept.
#[test]
fn an_exit_keeps_how_the_task_ended() {
    let t0 = Instant::now();
    let working = TaskMark::new(TaskState::Working)
        .apply(None, &Asks::default(), t0, 1_000)
        .0;
    let exit = TaskMark {
        why: Some(WHY_EXIT),
        exit: Some(0),
        ..TaskMark::new(TaskState::Exited)
    };

    let landed = TaskMark {
        why: Some(WHY_LANDED),
        checked: Some("abc@origin/main".to_string()),
        left: Some("kept:1".to_string()),
        ..TaskMark::new(TaskState::Done)
    }
    .apply(Some(&working), &Asks::default(), t0, 2_000)
    .0;
    let (gone, moved) = exit.apply(Some(&landed), &Asks::default(), t0, 3_000);
    assert!(moved);
    assert_eq!(gone.why, Some(WHY_LANDED));
    assert_eq!(gone.checked.as_deref(), Some("abc@origin/main"));
    assert_eq!(gone.left.as_deref(), Some("kept:1"));
    assert_eq!(gone.entered, landed.entered, "a look stays a look");
    assert_eq!(gone.conclusion(), Some(Conclusion::Finished));
    assert!(gone.finished);

    // WHATEVER WORD FINISHED IT: a hand's bare `done` and a marker's stand as a landing does, and
    // a carry row keeps the fact across a restart.
    for why in [None, Some("marker")] {
        let finished = TaskMark {
            why,
            ..TaskMark::new(TaskState::Done)
        }
        .apply(Some(&working), &Asks::default(), t0, 2_000)
        .0;
        let (gone, _) = exit.apply(Some(&finished), &Asks::default(), t0, 3_000);
        assert_eq!((gone.state, gone.why), (TaskState::Exited, why), "{why:?}");
        assert_eq!(gone.conclusion(), Some(Conclusion::Finished), "{why:?}");
        assert_eq!(gone.entered, finished.entered, "{why:?}");
        let row = gone.carry_row(t0);
        assert!(row.contains(" finished=1"), "{row}");
        let back = TaskRecord::from_carry_row(&row, None, t0, 3_000).expect("reads");
        assert_eq!(back.conclusion(), Some(Conclusion::Finished), "{row}");
        assert_eq!(back.carry_row(t0), row);
    }

    let failed = TaskMark {
        why: Some(WHY_COULD_NOT_FINISH),
        ..TaskMark::new(TaskState::Failed)
    }
    .apply(Some(&working), &Asks::default(), t0, 2_000)
    .0;
    let (gone, _) = exit.apply(Some(&failed), &Asks::default(), t0, 3_000);
    assert_eq!(gone.state, TaskState::Exited);
    assert_eq!(gone.why, Some(WHY_COULD_NOT_FINISH));
    assert_eq!(gone.entered, failed.entered);
    assert_eq!(gone.conclusion(), Some(Conclusion::Failed));

    let (gone, _) = exit.apply(Some(&working), &Asks::default(), t0, 3_000);
    assert_eq!(gone.why, Some(WHY_EXIT));
    assert_eq!(gone.conclusion(), Some(Conclusion::Failed));
    assert!(!gone.finished);
    assert!(!gone.carry_row(t0).contains("finished="));
    assert_ne!(gone.entered, working.entered);

    // A turn after the landing opens it again: an exit then reads as it stands.
    let open = TaskMark::new(TaskState::Working)
        .apply(Some(&landed), &Asks::default(), t0, 3_500)
        .0;
    let (gone, _) = exit.apply(Some(&open), &Asks::default(), t0, 4_000);
    assert_eq!(gone.conclusion(), Some(Conclusion::Failed));
}

/// A HAND'S `task mark failed` IS REFUSED WHILE A LOOP CARRIES THE SESSION: it would read as ended
/// while the loop types on, and the refusal says what stops it. NEGATIVE CONTROLS: Autopilot Off, no
/// claim, a lapsed claim, and aterm's own loop.
#[test]
fn a_hands_failed_mark_is_refused_while_a_loop_carries_the_session() {
    use crate::control::{Principal, Scope, ServingPrincipal};
    let h = crate::session_store::test_handle(9_401);
    let mark = |h: &crate::session_store::SessionHandle| {
        cmd_task(
            &h.ctx,
            h.local_id,
            Scope::Owner,
            "mark failed why=could-not-finish",
        )
    };
    let now = crate::metrics::now_us();
    crate::session_timeline::claim_supervisor(&h.ctx, "sup-a", None, Some(now + 60_000_000), now)
        .expect("claimed");
    h.ctx
        .meta
        .lock()
        .unwrap()
        .set_autopilot(Some("land".to_owned()), None);
    assert_eq!(mark(&h), FAILED_UNDER_A_LOOP);
    assert!(
        h.ctx.timeline.lock().unwrap().task().is_none(),
        "no record moved"
    );
    assert!(FAILED_UNDER_A_LOOP.contains("Autopilot off"));
    {
        let _harness = ServingPrincipal::enter(Principal::Harness);
        assert!(mark(&h).starts_with("OK task "), "aterm's own loop");
    }

    let h = crate::session_store::test_handle(9_402);
    assert!(mark(&h).starts_with("OK task "), "no claim");

    let h = crate::session_store::test_handle(9_403);
    let now = crate::metrics::now_us();
    crate::session_timeline::claim_supervisor(&h.ctx, "sup-a", None, Some(now + 60_000_000), now)
        .expect("claimed");
    h.ctx
        .meta
        .lock()
        .unwrap()
        .set_autopilot(Some("off".to_owned()), None);
    assert!(
        mark(&h).starts_with("OK task "),
        "Autopilot Off types nothing"
    );

    let h = crate::session_store::test_handle(9_404);
    let now = crate::metrics::now_us();
    crate::session_timeline::claim_supervisor(&h.ctx, "sup-a", None, Some(now + 1), now)
        .expect("claimed");
    h.ctx
        .meta
        .lock()
        .unwrap()
        .set_autopilot(Some("land".to_owned()), None);
    std::thread::sleep(Duration::from_millis(1));
    assert!(mark(&h).starts_with("OK task "), "a lapsed claim");
}

/// WHO HAS THEIR OWN REPORT OF THEIR END: a credit claim's worker and the disk worker, each by its
/// folder. NEGATIVE CONTROL: any other folder, and none.
#[test]
fn a_worker_with_its_own_report_is_told_by_its_folder() {
    use aterm_agent::harness::{credit_offers, disk};
    let under = |name: &str| format!("/state/aterm/{}/{name}", aterm_agent::harness::cli::HARNESS);
    assert!(reports_its_own_end(Some(&under(
        disk::fence::WORKER_FOLDER
    ))));
    assert!(reports_its_own_end(Some(&under(
        credit_offers::CLAIM_FOLDER
    ))));
    assert!(!reports_its_own_end(Some(&under("elsewhere"))));
    assert!(!reports_its_own_end(Some("/work/disk-worker")));
    assert!(!reports_its_own_end(None));
}

/// A PERSON'S AUTOPILOT OFF ENDS A WORKING TASK AND THEIR SWITCH BACK OPENS IT. aterm's own Off is
/// the expiry's, a word set before the record began is no move on it, and a failure of the worker's
/// own is not the word's to open. NEGATIVE CONTROL: Land over a working record moves nothing.
#[test]
fn autopilot_off_fails_a_working_task_and_the_switch_back_opens_it() {
    use crate::session_timeline::AutopilotSetBy;
    const LATER: u64 = 60_000;
    use aterm_agent::supervise::Autopilot;
    let t0 = Instant::now();
    let by = |actor, since_ms| AutopilotSetBy {
        actor,
        surface: None,
        since_ms,
    };
    let person = aterm_slog::Actor::Human;
    let working = TaskMark::new(TaskState::Working)
        .apply(None, &Asks::default(), t0, 1_000)
        .0;

    let off = autopilot_mark(
        &working,
        Some(Autopilot::Off),
        Some(&by(person, 2_000)),
        LATER,
    )
    .expect("a mark");
    assert_eq!((off.state, off.why), (TaskState::Failed, Some(WHY_OFF)));
    assert_eq!(
        autopilot_mark(
            &working,
            Some(Autopilot::Off),
            Some(&by(person, 500)),
            LATER
        ),
        None,
        "set before this task began"
    );
    assert_eq!(
        autopilot_mark(
            &working,
            Some(Autopilot::Off),
            Some(&by(aterm_slog::Actor::Aterm, 2_000)),
            LATER
        ),
        None,
        "aterm's own"
    );
    for hand in [aterm_slog::Actor::Ctl, aterm_slog::Actor::Bridge] {
        let off = autopilot_mark(
            &working,
            Some(Autopilot::Off),
            Some(&by(hand, 2_000)),
            LATER,
        )
        .expect("a mark");
        assert_eq!(
            (off.state, off.why),
            (TaskState::Failed, Some(WHY_OFF_CTL)),
            "only a Human reads as a person: {hand:?}"
        );
    }
    assert!(failed_words(TaskState::Failed, Some(WHY_OFF)).contains("person"));
    assert!(!failed_words(TaskState::Failed, Some(WHY_OFF_CTL)).contains("person"));
    assert_eq!(
        autopilot_mark(&working, Some(Autopilot::Off), None, LATER),
        None
    );
    assert_eq!(
        autopilot_mark(
            &working,
            Some(Autopilot::Land),
            Some(&by(person, 2_000)),
            LATER
        ),
        None
    );

    let failed_by = |why| {
        TaskMark {
            why: Some(why),
            ..TaskMark::new(TaskState::Failed)
        }
        .apply(Some(&working), &Asks::default(), t0, 2_000)
        .0
    };
    for why in [WHY_OFF, WHY_OFF_CTL, WHY_EXPIRED] {
        for word in [Autopilot::Land, Autopilot::Cruise] {
            let back = autopilot_mark(&failed_by(why), Some(word), Some(&by(person, 3_000)), LATER)
                .expect("opened");
            assert_eq!(back.state, TaskState::Working, "{why}");
        }
    }
    assert_eq!(
        autopilot_mark(
            &failed_by(WHY_COULD_NOT_FINISH),
            Some(Autopilot::Land),
            Some(&by(person, 3_000)),
            LATER
        ),
        None,
        "the worker's own word stands until a turn runs"
    );
    // Cruise never rests: set after any failure, it opens the task, the worker's own word too.
    for why in [WHY_COULD_NOT_FINISH, WHY_REPEATED] {
        let back = autopilot_mark(
            &failed_by(why),
            Some(Autopilot::Cruise),
            Some(&by(aterm_slog::Actor::Ctl, 3_000)),
            LATER,
        )
        .expect("opened");
        assert_eq!((back.state, back.why), (TaskState::Working, Some("turn")));
        assert_eq!(
            autopilot_mark(
                &failed_by(why),
                Some(Autopilot::Cruise),
                Some(&by(person, 1_500)),
                LATER
            ),
            None,
            "a Cruise set before it failed is not the move that opens it"
        );
    }
}

/// AN OFF FAILS THE TASK ONLY ONCE IT HAS STOOD: a click from Land through Off to Cruise tells a
/// waiter nothing, and the look owed when it settles fails it. NEGATIVE CONTROL: an Off stood
/// [`OFF_SETTLES_MS`] fails it, and nothing waits to settle once it has.
#[test]
fn an_off_fails_a_task_only_once_it_has_stood() {
    use crate::session_timeline::AutopilotSetBy;
    use aterm_agent::supervise::Autopilot;
    let set = AutopilotSetBy {
        actor: aterm_slog::Actor::Human,
        surface: None,
        since_ms: 2_000,
    };
    let working = TaskMark::new(TaskState::Working)
        .apply(None, &Asks::default(), Instant::now(), 1_000)
        .0;
    let at = |now| autopilot_mark(&working, Some(Autopilot::Off), Some(&set), now);
    assert_eq!(at(2_000), None, "a click passing through");
    assert_eq!(at(2_000 + OFF_SETTLES_MS - 1), None);
    assert_eq!(
        off_settles_in(&working, Some(Autopilot::Off), Some(&set), 2_500),
        Some(Duration::from_millis(OFF_SETTLES_MS - 500)),
        "a look is owed when it settles"
    );
    let off = at(2_000 + OFF_SETTLES_MS).expect("stood");
    assert_eq!((off.state, off.why), (TaskState::Failed, Some(WHY_OFF)));
    assert_eq!(
        off_settles_in(
            &working,
            Some(Autopilot::Off),
            Some(&set),
            2_000 + OFF_SETTLES_MS
        ),
        None
    );
    assert_eq!(
        off_settles_in(&working, Some(Autopilot::Cruise), Some(&set), 2_500),
        None,
        "it moved on: nothing to settle"
    );
}

/// A CLICK THAT PASSES THROUGH OFF ON ITS WAY TO CRUISE LEAVES A WORKING TASK WORKING, and the look
/// the window owes the Off finds nothing to fail once the word moved on.
#[test]
fn a_click_through_off_leaves_a_working_task_working() {
    use aterm_agent::supervise::Autopilot;
    let mut app = crate::App::headless_for_test();
    let local = app
        .focused_session_id(crate::WindowId(0))
        .expect("a session");
    let ctx = app.pool.get(local).expect("its session").ctx.clone();
    mark_session_task(&ctx, local, &TaskMark::new(TaskState::Working)).expect("unfenced");
    let set = |word: Autopilot| {
        let now_ms = i64::try_from(unix_ms_now()).expect("in range");
        let mut meta = ctx.meta.lock().unwrap();
        meta.autopilot = Some(word.word().to_owned());
        meta.autopilot_set_by = Some(crate::session_timeline::AutopilotSetBy {
            actor: aterm_slog::Actor::Human,
            surface: None,
            since_ms: now_ms,
        });
    };
    set(Autopilot::Off);
    app.hold_task_to_autopilot(local);
    let state = || ctx.timeline.lock().unwrap().task().map(|r| r.state);
    assert_eq!(state(), Some(TaskState::Working), "not yet stood");
    assert!(app.autopilot_off_wake().is_some(), "a look is owed");
    set(Autopilot::Cruise);
    app.tick_autopilot_offs(Instant::now() + Duration::from_millis(OFF_SETTLES_MS + 1));
    assert_eq!(state(), Some(TaskState::Working));
    assert_eq!(app.autopilot_off_wake(), None, "nothing left to settle");
}

/// A FAILED TASK'S RESULT READS AND A TURN INTO IT OPENS IT AGAIN, as a done task's does, and the
/// harness's own turn does not.
#[test]
fn a_failed_task_reads_its_result_and_a_turn_opens_it() {
    let handle = crate::session_store::test_handle(12);
    let ctx = &handle.ctx;
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-task-failed-")
        .tempdir()
        .expect("scratch");
    let transcript = dir.path().join("t.jsonl");
    let row = r#"{"type":"assistant","isSidechain":false,"uuid":"a-1","sessionId":"0b22","message":{"id":"msg_1","model":"claude-sonnet-5-5","content":[{"type":"text","text":"Could not finish: no grammar."}]}}"#;
    let snap = aterm_agent::harness::result::snapshot(
        &dir.path().join("tasks"),
        "s-12-1",
        Some((&transcript, &format!("{row}\n"))),
        aterm_agent::harness::result::DoneBy::Failed,
    )
    .expect("written");
    mark_session_task(ctx, 12, &TaskMark::new(TaskState::Working)).expect("unfenced");
    let failed = TaskMark {
        why: Some(WHY_COULD_NOT_FINISH),
        result_bytes: snap.bytes,
        result_path: Some(snap.path.clone()),
        ..TaskMark::new(TaskState::Failed)
    };
    mark_session_task(ctx, 12, &failed).expect("unfenced");
    assert!(
        cmd_result(ctx, 12, "").contains("Could not finish: no grammar."),
        "{}",
        cmd_result(ctx, 12, "")
    );
    let state = || {
        ctx.timeline
            .lock()
            .unwrap()
            .task()
            .map(|r| (r.state, r.why))
    };
    let found = || ctx.timeline.lock().unwrap().task().map(|r| r.entered);
    reopen_at_turn(ctx, 12, &TurnBy::Harness, found());
    assert_eq!(
        state(),
        Some((TaskState::Failed, Some(WHY_COULD_NOT_FINISH)))
    );
    reopen_at_turn(ctx, 12, &TurnBy::Ctl, found());
    assert_eq!(state(), Some((TaskState::Working, Some("turn"))));
}
