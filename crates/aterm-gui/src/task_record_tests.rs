// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for task record grammar, idempotent marks, handoff rows, and `await task` parsing.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::*;

const SID: &str = "s-0123456789abcdef0123";

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
    assert!(
        parse("done why=because")
            .unwrap_err()
            .contains("why= is one of landed|marker|done-check|exit|shell|startup|held|turn")
    );
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
    let (first, moved) = working.apply(None, false, t0, 1_000);
    assert!(moved);
    assert_eq!(
        (first.rev, first.state, first.from.as_str()),
        (1, TaskState::Working, SID)
    );
    assert_eq!(first.since_unix_ms, 1_000);

    let (again, moved) = working.apply(Some(&first), false, t0 + Duration::from_secs(1), 2_000);
    assert!(!moved);
    assert_eq!(again, first, "no rev, no since moved");
    let (kept, moved) = TaskMark::new(TaskState::Working).apply(Some(&first), false, t0, 2_000);
    assert!(!moved && kept.from == SID);

    let mut done = TaskMark::new(TaskState::Done);
    done.why = Some("landed");
    done.result_bytes = 42;
    let (finished, moved) = done.apply(Some(&first), false, t0 + Duration::from_secs(2), 3_000);
    assert!(moved);
    assert_eq!((finished.rev, finished.state), (2, TaskState::Done));
    assert_eq!((finished.since_unix_ms, finished.result_bytes), (3_000, 42));
    assert_eq!(finished.from, SID, "the record keeps its origin");
    let (same, moved) = done.apply(Some(&finished), false, t0 + Duration::from_secs(9), 9_000);
    assert!(
        !moved && same.rev == 2,
        "a harness marking done again wakes no one"
    );

    let (resized, moved) = TaskMark::new(TaskState::Done).apply(
        Some(&finished),
        false,
        t0 + Duration::from_secs(3),
        4_000,
    );
    assert!(moved);
    assert_eq!((resized.rev, resized.since_unix_ms), (3, 3_000));
    assert_eq!((resized.why, resized.result_bytes), (None, 0));
    let (raised, moved) = TaskMark::new(TaskState::Done).apply(
        Some(&resized),
        true,
        t0 + Duration::from_secs(4),
        5_000,
    );
    assert!(moved && raised.rev == 4 && raised.attention && raised.has_news());
}

#[test]
fn the_carried_row_round_trips_and_a_bad_row_is_none() {
    let t0 = Instant::now();
    let record = TaskRecord {
        rev: 7,
        state: TaskState::Exited,
        from: SID.to_string(),
        why: Some("exit"),
        exit: Some(-2),
        result_bytes: 9,
        result_path: Some(tasks().join("r 1.txt")),
        attention: true,
        since_unix_ms: 10_000,
        since: t0,
        entered: 7,
        agent: Some(aterm_phase::Program::Agy),
        trusted_cwd: Some(folder("w/my proj")),
        fact: None,
        fact_rev: 0,
    };
    let row = record.carry_row();
    let (wire, trusted) = if cfg!(windows) {
        (r"C:\state\aterm\tasks\r%201.txt", r"C:\w\my%20proj")
    } else {
        ("/state/aterm/tasks/r%201.txt", "/w/my%20proj")
    };
    assert_eq!(
        row,
        format!(
            "rev=7 state=exited from={SID} since=10000 result_bytes=9 attention=1 why=exit \
             exit=-2 result_path={wire} agent=agy trusted_cwd={trusted}"
        )
    );
    let later = t0 + Duration::from_secs(60);
    let back = TaskRecord::from_carry_row(&row, Some(&tasks()), later, 15_000).expect("reads");
    assert_eq!(back.since, later - Duration::from_secs(5));
    assert_eq!(back.carry_row(), row, "the row reads back to its own bytes");
    assert_eq!(TaskRecord { since: t0, ..back }, record);
    let bare = TaskRecord {
        why: None,
        exit: None,
        result_path: None,
        attention: false,
        agent: None,
        trusted_cwd: None,
        ..record.clone()
    };
    let back = TaskRecord::from_carry_row(&bare.carry_row(), Some(&tasks()), t0, 10_000).unwrap();
    assert_eq!(back, bare);
    let held = TaskRecord {
        state: TaskState::Stalled,
        why: Some("held"),
        ..bare.clone()
    };
    assert_eq!(
        TaskRecord::from_carry_row(&held.carry_row(), Some(&tasks()), t0, 10_000),
        Some(held)
    );
    let newer = format!("{} model=opus pinned", bare.carry_row());
    assert_eq!(
        TaskRecord::from_carry_row(&newer, Some(&tasks()), t0, 10_000),
        Some(bare.clone())
    );
    for agent in aterm_phase::Program::ALL {
        let with = TaskRecord {
            agent: Some(agent),
            ..bare.clone()
        };
        let back = TaskRecord::from_carry_row(&with.carry_row(), Some(&tasks()), t0, 10_000)
            .expect("reads");
        let spawned = aterm_agent::harness::worker::worker_cli(agent).is_some();
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
        TaskRecord::from_carry_row(&with.carry_row(), Some(&tasks()), t0, 10_000),
        Some(with.clone())
    );
    for bad in ["w/proj", "/w/../etc", "/w/proj/", "/w/a%0Ab", "/w//proj"] {
        let row = format!("{} trusted_cwd={bad}", bare.carry_row());
        assert_eq!(
            TaskRecord::from_carry_row(&row, Some(&tasks()), t0, 10_000),
            Some(bare.clone()),
            "{bad}"
        );
    }
    let unknown = format!("{} agent=a-later-cli", bare.carry_row());
    assert_eq!(
        TaskRecord::from_carry_row(&unknown, Some(&tasks()), t0, 10_000),
        Some(bare.clone())
    );
    for retired in aterm_phase::RETIRED_AGENTS {
        let carried = format!("{} agent={retired}", bare.carry_row());
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
        why: Some("landed"),
        exit: None,
        result_bytes: 120,
        result_path: None,
        attention: false,
        since_unix_ms: 0,
        since: Instant::now(),
        entered: 3,
        agent: Some(aterm_phase::Program::Agy),
        trusted_cwd: Some(folder("w")),
        fact: None,
        fact_rev: 0,
    };
    let fields = record.wire_fields();
    assert!(
        fields.starts_with("rev=3 state=done from=ctl since_ms="),
        "{fields}"
    );
    assert!(fields.ends_with(" result_bytes=120 why=landed"), "{fields}");
    assert_eq!(status_tokens(Some(&record)), "task_state=done task_rev=3");
    assert_eq!(status_tokens(None), "task_state=- task_rev=0");
    assert_eq!(
        await_row(SID, Some(&record), "idle", "-", "-", false),
        format!("@{SID} rev=3 state=done agent=idle program_state=- attention=- result_bytes=120")
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
    write(needs, MetaEdit::Clear);
    assert!(flag().1, "the supervisor's entry still stands");
    write(supervisor, MetaEdit::Clear);
    assert!(!flag().1, "both cleared");
    let codex = crate::task_record::SELF_CLAIM_ATTENTION_OWNER;
    assert!(crate::task_record::owner_moves_task(codex));
    write(codex, MetaEdit::Set("box: Allow this command?"));
    assert!(flag().1, "a point codex-alab left to a person raises it");
    write(codex, MetaEdit::Clear);
    assert!(!flag().1, "and its clearing lowers it");
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
            " result_bytes=7 why=landed agent=- program_state=- attention=stuck%20on%20CI\n"
        ),
        "{read}"
    );
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
        why: None,
        exit: None,
        result_bytes: 0,
        result_path: None,
        attention: false,
        since_unix_ms: 10_000,
        since: t0,
        entered: 1,
        agent: None,
        trusted_cwd: None,
        fact: hold_room(2),
        fact_rev: 3,
    };
    let row = record.carry_row();
    assert_eq!(
        row,
        "rev=4 state=working from=ctl since=10000 result_bytes=0 attention=0 \
         fact=hold-room@v1#2 fact_rev=3"
    );
    let back = TaskRecord::from_carry_row(&row, None, t0, 10_000).expect("reads");
    assert_eq!((back.fact, back.fact_rev), (hold_room(2), 3));
    assert_eq!(back.carry_row(), row);
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
    let row = cleared.carry_row();
    assert!(row.ends_with(" attention=0 fact_rev=3"), "{row}");
    let back = TaskRecord::from_carry_row(&row, None, t0, 10_000).expect("reads");
    assert_eq!((back.fact, back.fact_rev), (None, 3));
    let older = row.replace(" fact_rev=3", "");
    let bare = TaskRecord::from_carry_row(&older, None, t0, 10_000).expect("an older row reads");
    assert_eq!((bare.fact, bare.fact_rev), (None, 0));
    assert_eq!(bare.carry_row(), older, "an unedited word carries nothing");
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
    let twice = format!("{} fact_rev=4", record.carry_row());
    assert_eq!(TaskRecord::from_carry_row(&twice, None, t0, 10_000), None);
    let twice = format!("{} fact=hold-room@v1#1", record.carry_row());
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
        let record = ctx.timeline.lock().unwrap().task().cloned();
        let (agent, program, attention) = agent_program_and_attention(ctx);
        [
            cmd_task(ctx, 15, Scope::Owner, ""),
            await_row(SID, record.as_ref(), agent, &program, &attention, false),
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
