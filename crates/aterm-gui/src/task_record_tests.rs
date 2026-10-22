// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The task record's grammar, its idempotent mark, its handoff row and the
//! `await task` argument parse. The waits themselves are driven in
//! `control_session`'s tests, where `await_task` lives.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::*;

const SID: &str = "s-0123456789abcdef0123";

/// `unix`, a path spelled as on Unix, spelled as this platform spells it:
/// on Windows an absolute one is on drive `C:`, and every `/` is a `\`.
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

/// The absolute folder `/<rest>`, as this platform spells it.
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

/// THE LOG'S TASK WORDS MIRROR THE TASK RECORD'S (plan 5C-1, ruling T16:
/// a mirror of an aterm-gui vocabulary lives in aterm-gui). Every task state
/// is a `state=` word of the session log and every `why=` word a `why=`
/// word, so a `task` mark ([`crate::session_timeline::SessionTimeline::mark_task`])
/// never writes one as `unknown`. The match is exhaustive: a fifth state is a
/// compile error here until the log knows its word. NEGATIVE CONTROL: a word
/// the log's lists do not hold is refused by the same lookup.
#[test]
fn the_logs_task_words_mirror_the_task_record() {
    use aterm_slog::vocab::{STATE_WORDS, WHY_WORDS, lookup};
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
    assert_eq!(lookup(STATE_WORDS, "finished"), None);
    assert_eq!(lookup(WHY_WORDS, "because"), None);
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
    // An older build's done words still parse (a record a seamless update or
    // a rollback carried); no harness of this build writes them.
    for old in ["marker", "done-check"] {
        assert_eq!(
            parse(&format!("done why={old}")).unwrap().why,
            Some(old),
            "{old}"
        );
    }
    // A path with a space travels pct-encoded.
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
    // NEGATIVE CONTROLS for the path: outside the dir, the dir itself, a
    // climb out, a relative path, a control character, no state dir at all.
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
    // Windows: rooted on no drive, on another drive, the verbatim spelling,
    // and the same second spellings with `/` for a separator.
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
    // At most `RESULT_PATH_MAX` bytes decoded: macOS's PATH_MAX.
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

    // IDEMPOTENT: the same state with the same facts is no change.
    let (again, moved) = working.apply(Some(&first), false, t0 + Duration::from_secs(1), 2_000);
    assert!(!moved);
    assert_eq!(again, first, "no rev, no since moved");
    // `from` is kept when a mark names none.
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

    // A fact that changes within the state bumps the rev, not the since; a
    // fact left out is cleared.
    let (resized, moved) = TaskMark::new(TaskState::Done).apply(
        Some(&finished),
        false,
        t0 + Duration::from_secs(3),
        4_000,
    );
    assert!(moved);
    assert_eq!((resized.rev, resized.since_unix_ms), (3, 3_000));
    assert_eq!((resized.why, resized.result_bytes), (None, 0));
    // The attention flag is part of the record.
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
    };
    let row = record.carry_row();
    // The paths ride in this platform's own spelling, pct-encoded.
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
    // Read back five seconds of wall clock later: the same record, and its
    // `since_ms=` keeps counting from when the state was entered.
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
    // The held deadline's stall crosses a seamless update as it stands.
    let held = TaskRecord {
        state: TaskState::Stalled,
        why: Some("held"),
        ..bare.clone()
    };
    assert_eq!(
        TaskRecord::from_carry_row(&held.carry_row(), Some(&tasks()), t0, 10_000),
        Some(held)
    );
    // A NEWER BUILD'S ROW still reads after a rollback: a key this build does
    // not know, or a bare word, is skipped, never the record.
    let newer = format!("{} model=opus pinned", bare.carry_row());
    assert_eq!(
        TaskRecord::from_carry_row(&newer, Some(&tasks()), t0, 10_000),
        Some(bare.clone())
    );
    // THE WORKER'S AGENT CROSSES (each one a spawn starts), and one this
    // build does not know (a newer build's agent, or no worker's: `codex`)
    // costs the agent alone, never the record.
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
    // THE FOLDER ITS SPAWN TRUSTED CROSSES, and one this build cannot vouch
    // for (relative, climbing, a second spelling, a control character) costs
    // the folder alone, never the record: no folder is trusted on it.
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
        agent: Some(aterm_phase::Program::Gemini),
        trusted_cwd: Some(folder("w")),
    };
    let fields = record.wire_fields();
    assert!(
        fields.starts_with("rev=3 state=done from=ctl since_ms="),
        "{fields}"
    );
    assert!(fields.ends_with(" result_bytes=120 why=landed"), "{fields}");
    assert_eq!(
        record.event_payload(),
        "rev=3 state=done attention=0 why=landed"
    );
    assert_eq!(status_tokens(Some(&record)), "task_state=done task_rev=3");
    assert_eq!(status_tokens(None), "task_state=- task_rev=0");
    assert_eq!(
        await_row(SID, Some(&record), "idle", "-", false),
        format!("@{SID} rev=3 state=done agent=idle attention=- result_bytes=120")
    );
    assert_eq!(
        await_row(SID, None, "-", "stuck%20on%20CI", false),
        format!("@{SID} rev=0 state=- agent=- attention=stuck%20on%20CI result_bytes=0")
    );
    assert!(await_row(SID, Some(&record), "-", "-", true).contains(" state=closed "));
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
                    since: Some(4)
                },
                TargetArg {
                    selector: "2".into(),
                    since: None
                },
                TargetArg {
                    selector: ".".into(),
                    since: None
                },
            ],
            combine: Combine::All,
            since: Some(3),
            timeout_ms: Some(1500),
        }
    );
    // The single form: no list, `any` the default, `timeout <ms>` spelled
    // either way, and the server's 600 s clamp.
    let single = parse_await_task(&words("timeout 999999999"), false).unwrap();
    assert_eq!(single.combine, Combine::Any);
    assert_eq!(single.timeout_ms, Some(aterm_control::wire::WAIT_MAX_MS));
    assert_eq!(parse_await_task(&[], false).unwrap().targets, Vec::new());
    assert_eq!(
        parse_await_task(&words("any"), false).unwrap().combine,
        Combine::Any
    );

    for usage in [
        "@s-a", // a list where the selector already named one
        "since=x",
        "since=1 since=2",
        "any all",
        "timeout=soon",
        "timeout",
        "later",
    ] {
        assert_eq!(
            parse_await_task(&words(usage), false),
            Err(AWAIT_TASK_USAGE.to_string()),
            "{usage:?}"
        );
    }
    for usage in ["@s-a @s-b", "@s-a:x", "@s-a,s-b", "@s-a:"] {
        assert_eq!(
            parse_await_task(&words(usage), true),
            Err(AWAIT_TASK_USAGE.to_string()),
            "{usage:?}"
        );
    }
    // An element naming nothing (`@$W` with `$W` empty) is the empty-selector
    // refusal, never the front session; `@.` written out is a target.
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

/// The in-process API and the verb share one path: a mark records its
/// `task-change` row (the `events` digest's `EVENT <local> task`), the same
/// mark again records none, and the supervisor's attention moves a tasked
/// session's rev but never gives an untasked one a record.
#[test]
fn a_mark_records_its_row_and_the_supervisors_attention_moves_the_rev() {
    use crate::session_timeline::{MetaEdit, write_attention_owned};

    let handle = crate::session_store::test_handle(4);
    let owner = crate::harness_host::ATTENTION_OWNER;
    // No task: the supervisor's attention gives the session no record.
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
    // Another owner's entry is not the supervisor's news.
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
    // A mark reads the flag under the meta guard: raised before the mark, the
    // record it makes says so.
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

/// A WORKER'S NEEDS YOU ENTRY IS ITS TASK RECORD'S NEWS (FOREVER LAND: "be
/// LOUDER ABOUT IT ... but NEVER let that block you"): an `owner=needs` entry
/// raises a tasked session's attention and moves its rev as the supervisor's
/// own does, its clear lowers it, and the flag stands while either entry
/// does. NEGATIVE CONTROL: a person's own entry moves nothing.
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
    // NEGATIVE CONTROL: a person's own entry.
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

/// `task` reads the record with the session's agent word and attention;
/// `task mark` answers the record after it and whether it moved; an edge
/// scope is refused the mark; and a stray word is the usage line.
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
        marked.ends_with(" result_bytes=0 agent=- attention=- changed=1\n"),
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
        read.ends_with(" result_bytes=7 why=landed agent=- attention=stuck%20on%20CI\n"),
        "{read}"
    );
}

/// A DONE OVER A DEADLINE'S STALL IS ONE EDGE: a worker the window host
/// stalled `why=held`, the deadline's attention raised with it, that moved
/// and finished after all — its harness publishes `done` over the stall —
/// gets one rev that carries the done and `attention=-`, and the host's
/// backstop (`StartupMark::Over`'s take-down) finds nothing left to move, so
/// a manager re-armed past that rev is not woken again with no news.
/// NEGATIVE CONTROLS: a mark to the deadline's own stall keeps its text, and
/// another text of the supervisor's (a wall it escalated) stays through a
/// done.
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

    // NEGATIVE CONTROL: the deadline's own stall, marked over the wire.
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
    // NEGATIVE CONTROL: another text of the supervisor's stays.
    raise("a usage limit until 5pm");
    let done = cmd_task(ctx, 6, Scope::Owner, "mark done why=landed");
    assert!(done.contains(" attention=a%20usage%20limit"), "{done}");
}

/// A path is lexical: `result_path` is never opened here.
#[test]
fn a_result_path_is_judged_lexically() {
    let dir = native("/nonexistent/tasks");
    let dir = Some(Path::new(&dir));
    let inside = native("/nonexistent/tasks/a/b.txt");
    assert_eq!(result_path(&inside, dir), Some(PathBuf::from(&inside)));
    assert_eq!(result_path(&native("/nonexistent/tasksx/b.txt"), dir), None);
}

/// A WINDOWS STATE DIR: a result under `%LOCALAPPDATA%\aterm\tasks` is
/// judged under it, a space travelling pct-encoded, and kept as spelled; so
/// is one under a state root spelled with `/` (`ATERM_STATE_HOME=C:/…`),
/// which `tasks\<file>` joins with `\`. NEGATIVE CONTROLS: the drive's
/// relative spelling, another drive, a prefix of the dir's last name.
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

/// THE WORKER'S RESULT (`result [full]`): the snapshot the record names,
/// answered with its stamp and its text; `ERR no task` with no record, and a
/// record with no result says why in its own state's word — `exited` with
/// none, but an `exited` worker the harness had marked done keeps its
/// result. A `done` with no file named is `no-snapshot`. A stray word is the
/// usage line.
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
    // The agent gone, its result kept.
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

/// A RESULT THE DONE EDGE MISSED IS READ AGAIN BY `result`, FOR ITS TURN: a
/// `done` whose snapshot said `no-reply` (the harness read the transcript
/// before Claude Code wrote the reply) answers the miss while the reply is
/// not there; once it is, `result` answers it, the record says its size
/// under a new rev, and the snapshot holds it — a second `result` reads
/// nothing again and moves nothing. NEGATIVE CONTROL: a reply that only a
/// later turn holds is never the task's.
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
    // NEGATIVE CONTROL: only a later turn has a reply.
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

/// A TURN INTO A FINISHED WORKER opens its task again at once
/// ([`reopen_at_turn`]): typed by `aterm ctl`, a peer or the bridge, the
/// `done` record its Enter found moves to `working why=turn` with the next
/// rev, and a second such turn changes nothing more. NEGATIVE CONTROLS: the
/// window harness's own turn (the live upgrade's READY notice) leaves `done`
/// as it stands; a record in any other state, and a session with no task,
/// are left alone; and so is a `done` the Enter did not find — the harness's
/// `working` and `done` for this very turn landing before its submit was
/// verified — which this turn's own reopen would otherwise hide from every
/// `await task` past the rev it found.
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
    // What an Enter finds: the entry into the state the record stands in.
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
    // The Enter found a done; the harness's working and done for the turn
    // land before its submit is verified: that done stands.
    mark_session_task(ctx, 7, &done("landed")).expect("unfenced");
    let entered = found();
    let mut turn = TaskMark::new(TaskState::Working);
    turn.why = Some("turn");
    mark_session_task(ctx, 7, &turn).expect("unfenced");
    mark_session_task(ctx, 7, &done("landed")).expect("unfenced");
    let published = state();
    reopen_at_turn(ctx, 7, &TurnBy::Ctl, entered);
    assert_eq!(state(), published, "the done the Enter did not find stands");
    // NEGATIVE CONTROL: the done it found opens.
    reopen_at_turn(ctx, 7, &TurnBy::Ctl, found());
    assert_eq!(state().map(|s| s.1), Some(TaskState::Working));
    mark_session_task(ctx, 7, &TaskMark::new(TaskState::Stalled)).expect("unfenced");
    let before = state();
    reopen_at_turn(ctx, 7, &TurnBy::Ctl, found());
    assert_eq!(state(), before, "only a done task is opened again");
}

/// THE DONE THE ENTER FOUND, MARKED AGAIN, STILL OPENS (px3a's review, the
/// race): a follow-up's Enter finds `done` at rev n, and before its submit is
/// verified a `result` that read the turn again keeps the reply it found
/// ([`repair_result`]: the same done, `result_bytes=` moved, rev n+1) — or
/// the supervisor's attention is raised on it (n+2). The verified submit
/// opens THAT done all the same, so the record reads `working why=turn` past
/// the rev the turn's verdict names, and an `await task @w:<n>` armed on it
/// waits for the follow-up's own done instead of answering at once with the
/// re-mark (keyed on the rev alone, as the reopen first was, it missed: the
/// record's rev is not n any more). NEGATIVE CONTROL: an entry the Enter did
/// not find — the `working` before that done — opens nothing.
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
    // The Enter: its verdict will name rev n.
    let at_enter = record();
    let n = at_enter.rev;
    // The lag repair, conditional on the record it read (`repair_result`).
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
    // A waiter past n would answer at once on this: news.
    assert!(remarked.rev > n && remarked.has_news());
    // NEGATIVE CONTROL: another entry than the one the Enter found.
    reopen_at_turn(ctx, 7, &TurnBy::Ctl, Some(at_enter.entered - 1));
    assert_eq!(
        record(),
        remarked,
        "an entry the Enter did not find opens nothing"
    );
    // The verified submit opens the done its Enter found.
    reopen_at_turn(ctx, 7, &TurnBy::Ctl, Some(at_enter.entered));
    let opened = record();
    assert_eq!(
        (opened.rev, opened.state, opened.why),
        (n + 3, TaskState::Working, Some("turn"))
    );
    // Past n the record is working with the attention a person must see —
    // news only for that — and with it taken down nothing answers until the
    // follow-up's own done.
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

/// THE REOPEN IS CONDITIONAL UNDER THE MARK'S OWN LOCKS
/// ([`mark_session_task_if`]): the move to `working` applies only to the
/// record as it stands when the mark lands, so an `exited` the host marked
/// after the turn's first look stands. NEGATIVE CONTROL: the same mark over
/// a record that still reads `done` moves it.
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
    // NEGATIVE CONTROL: still `done`.
    mark_session_task(ctx, 7, &TaskMark::new(TaskState::Done)).expect("unfenced");
    let landed = mark_session_task_if(ctx, 7, &working, Duration::ZERO, is_done).expect("unfenced");
    assert_eq!(
        landed.map(|(r, changed)| (r.state, changed)),
        Some((TaskState::Working, true))
    );
    assert_eq!(state(), Some((3, TaskState::Working)));
}

/// A RECORD THAT ENTERS A STATE RINGS THE WINDOW HOST (its deadlines judge a
/// worker by its record's state): the `working why=turn` a follow-up's
/// verified submit opens lands after the ring of the screen it drew, and the
/// held deadline's look must come for it — the live run of the held worker
/// waited out its hang detector without it. NEGATIVE CONTROLS: the same
/// state marked again (a lag repair's `result_bytes=`), a mark that changes
/// nothing, and a conditional mark that does not land ring nothing.
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
    // NEGATIVE CONTROLS.
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
    // The follow-up's reopen enters `working` again: rung.
    reopen_at_turn(ctx, 7, &TurnBy::Ctl, Some(2));
    assert_eq!(
        rings() - before,
        1,
        "{:?}",
        ctx.timeline.lock().unwrap().task()
    );
}

/// THE TASK WAKE (`docs/DESIGN-owner-surfaces-2026-10-16.md` §9 S3): exactly
/// one `Wake::TaskChanged` per mark that moves the record or the supervisor's
/// attention it carries, so the need verdict — a `stalled` task is a need —
/// is published again on the main thread. NEGATIVE CONTROLS: a mark that
/// changes nothing, a conditional mark that does not land, and a mark a
/// committing update's fence holds back post none.
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
    // A log wake another writer left owed (one that does not take its own,
    // as a `human` after-mail mark written on a try-lock does), taken by the
    // same mark again: the session's waiters wake for it, but the record did
    // not move, so no need verdict is posted.
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
    // The supervisor's attention carried by a mark, alone, moves the record.
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
    // A conditional mark that does not land.
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
    // A committing update's fence: the mark is refused, nothing moves.
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
