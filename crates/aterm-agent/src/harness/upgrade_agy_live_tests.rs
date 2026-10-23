// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Unit tests for the pure `agy` quiet-upgrade eligibility checks.

use super::*;

/// The conversation, and a subagent of it.
const CONV: &str = "a1047651-e950-4c37-9043-83382fad0eb9";
const SUB: &str = "f9935f58-a2d9-4f58-bb63-1f8d3ffa0d23";
const SUB2: &str = "7ef6fda5-35fe-4eaa-a143-b167723cab1f";
/// The process's start (unix ms) and a row time after it.
const START_MS: u64 = 1_792_349_000_000;
const AT: &str = "2026-10-18T18:43:30Z";
const BEFORE: &str = "2026-10-18T18:40:00Z";

/// One transcript row as agy writes it.
fn row(kind: &str, status: &str, at: &str, content: &str) -> String {
    let source = match kind {
        "SYSTEM_MESSAGE" => "SYSTEM",
        "USER_INPUT" => "USER_EXPLICIT",
        _ => "MODEL",
    };
    format!(
        r#"{{"step_index":1,"source":"{source}","type":"{kind}","status":"{status}","created_at":"{at}","content":{}}}"#,
        aterm_json::to_string(&Value::from(content)).expect("content")
    )
}

/// A planner row calling `name` with `args`, each value JSON-in-a-string as
/// agy keeps it.
fn call(name: &str, args: &[(&str, &str)], at: &str) -> String {
    let args: Vec<String> = args
        .iter()
        .map(|(k, v)| {
            format!(
                "{}:{}",
                aterm_json::to_string(&Value::from(*k)).expect("k"),
                aterm_json::to_string(&Value::from(*v)).expect("v")
            )
        })
        .collect();
    format!(
        r#"{{"step_index":2,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"{at}","tool_calls":[{{"name":"{name}","args":{{{}}}}}]}}"#,
        args.join(",")
    )
}

fn task_started(n: u32, what: &str, at: &str) -> String {
    row(
        "GENERIC",
        "RUNNING",
        at,
        &format!(
            "Created At: 2026-10-18T11:43:30-07:00\nTool is running as a background task with task id: {CONV}/task-{n}\nTask Description: {what}\nTask logs are available at: file:///h/.gemini/antigravity-cli/brain/{CONV}/.system_generated/tasks/task-{n}.log"
        ),
    )
}

fn message(sender: &str, priority: &str, content: &str, at: &str) -> String {
    row(
        "SYSTEM_MESSAGE",
        "DONE",
        at,
        &format!(
            "The following is a <SYSTEM_MESSAGE> not actually sent by the user. It is provided by the system as important information to pay attention to.\n\n<SYSTEM_MESSAGE>\n[Message] timestamp={at} sender={sender} priority={priority} content={content}\n</SYSTEM_MESSAGE>"
        ),
    )
}

fn task_ended(n: u32, at: &str) -> String {
    message(
        &format!("{CONV}/task-{n}"),
        "MESSAGE_PRIORITY_HIGH",
        &format!(
            "Task id \"{CONV}/task-{n}\" finished with result:\n\nThe command exited with code 0.\nOutput:\nLAB-BG-DONE\r\n"
        ),
        at,
    )
}

fn spawned(id: &str, at: &str) -> String {
    row(
        "GENERIC",
        "DONE",
        at,
        &format!(
            "Created At: 2026-10-18T11:49:52-07:00\nCompleted At: 2026-10-18T11:49:52-07:00\nCreated the following subagents:\n{{\n  \"conversationId\":  \"{id}\",\n  \"logAbsoluteUri\":  \"file:///h/.gemini/antigravity-cli/brain/{id}/.system_generated/logs/transcript.jsonl\",\n  \"workspaceUris\":  [\n    \"file:///p\"\n  ]\n}}\nThe subagents will send you a message when they have completed their task or require guidance. There is no need to poll for their responses."
        ),
    )
}

fn idled(id: &str, at: &str) -> String {
    message(
        "system",
        "MESSAGE_PRIORITY_HIGH",
        &format!("Subagent {id} has gone idle. This was the subagent's most recent response: DONE"),
        at,
    )
}

fn killed_subagents(at: &str) -> String {
    row(
        "GENERIC",
        "DONE",
        at,
        "Created At: 2026-10-18T11:49:57-07:00\nCompleted At: 2026-10-18T11:49:57-07:00\nSuccessfully killed 1 subagent(s) and their descendants.\nKilled roles: Lab Waiter",
    )
}

fn turn(at: &str) -> String {
    [
        row(
            "USER_INPUT",
            "DONE",
            at,
            "<USER_REQUEST>\ntoolstart\n</USER_REQUEST>",
        ),
        row("PLANNER_RESPONSE", "DONE", at, "Tool call done."),
    ]
    .join("\n")
}

fn log(rows: &[String]) -> String {
    rows.join("\n") + "\n"
}

fn read_from() -> u64 {
    START_MS / 1000 - START_SLACK_S
}

/// G-A2(b), TASKS: a command task, a timer and a daemon task whose `RUNNING`
/// row stands with no end hold the restart; each one's end row — the
/// `SYSTEM_MESSAGE` its `sender=` names, or a `manage_task` kill's `Task
/// "<id>" cancelled.` — ends it. NEGATIVE CONTROLS: another task's end ends
/// nothing, and a LOW message of the cancelled task arriving later is no new
/// task.
#[test]
fn a_background_task_holds_the_restart_until_its_end_row() {
    let running = log(&[turn(AT), task_started(4, "sleep 150; echo LAB-BG-DONE", AT)]);
    assert_eq!(open_work(&running, read_from()), Err(Open::Task));
    let ended = log(&[
        turn(AT),
        task_started(4, "sleep 150; echo LAB-BG-DONE", AT),
        task_ended(4, AT),
    ]);
    assert_eq!(open_work(&ended, read_from()), Ok(()));
    let other = log(&[
        turn(AT),
        task_started(4, "sleep 150", AT),
        task_ended(9, AT),
    ]);
    assert_eq!(open_work(&other, read_from()), Err(Open::Task));
    let timer = log(&[
        task_started(10, "Timer: 90s, Prompt: lab timer fired", AT),
        message(
            &format!("{CONV}/task-10"),
            "MESSAGE_PRIORITY_HIGH",
            "lab timer fired",
            AT,
        ),
    ]);
    assert_eq!(open_work(&timer, read_from()), Ok(()));
    let daemon = log(&[task_started(35, "sleep 300", AT)]);
    assert_eq!(open_work(&daemon, read_from()), Err(Open::Task));
    let cancelled = log(&[
        task_started(27, "sleep 200; echo LAB-KILLME", AT),
        call(
            "manage_task",
            &[
                ("Action", "\"kill\""),
                ("TaskId", &format!("\"{CONV}/task-27\"")),
            ],
            AT,
        ),
        row(
            "GENERIC",
            "DONE",
            AT,
            &format!(
                "Created At: 2026-10-18T11:49:08-07:00\nCompleted At: 2026-10-18T11:49:08-07:00\nTask \"{CONV}/task-27\" cancelled."
            ),
        ),
        message(
            &format!("{CONV}/task-27"),
            "MESSAGE_PRIORITY_LOW",
            "was canceled",
            AT,
        ),
    ]);
    assert_eq!(open_work(&cancelled, read_from()), Ok(()));
}

/// A CRON runs on across its firings (each a `sender=` row, `(iteration N)`):
/// with no `MaxIterations` it is never ended by one; with `MaxIterations` it
/// is ended by that many; and a `manage_task` kill ends it whatever. Its
/// schedule call is matched to its task by expression and prompt. NEGATIVE
/// CONTROL: a timer's firing ends it at once.
#[test]
fn a_cron_holds_the_restart_until_its_last_firing() {
    let schedule = |max: Option<&str>| {
        let mut args = vec![
            ("CronExpression", "\"*/1 * * * *\""),
            ("Prompt", "\"lab cron fired\""),
        ];
        if let Some(max) = max {
            args.push(("MaxIterations", max));
        }
        call("schedule", &args, AT)
    };
    let fired = |k: u32| {
        message(
            &format!("{CONV}/task-44"),
            "MESSAGE_PRIORITY_HIGH",
            &format!("lab cron fired (iteration {k})"),
            AT,
        )
    };
    let started = task_started(44, "Cron: */1 * * * *, Prompt: lab cron fired", AT);
    let standing = log(&[schedule(None), started.clone(), fired(1), fired(2)]);
    assert_eq!(open_work(&standing, read_from()), Err(Open::Task));
    let once = log(&[schedule(Some("1")), started.clone(), fired(1)]);
    assert_eq!(open_work(&once, read_from()), Ok(()));
    let twice = log(&[schedule(Some("2")), started.clone(), fired(1)]);
    assert_eq!(open_work(&twice, read_from()), Err(Open::Task));
    let twice_done = log(&[schedule(Some("2")), started.clone(), fired(1), fired(2)]);
    assert_eq!(open_work(&twice_done, read_from()), Ok(()));
    let unmatched = log(&[started.clone(), fired(1)]);
    assert_eq!(
        open_work(&unmatched, read_from()),
        Err(Open::Task),
        "a cron with no schedule call read runs until cancelled"
    );
}

/// G-A2(b), SUBAGENTS: a spawned subagent holds the restart until its `has
/// gone idle` row, or a `manage_subagents` kill naming it (or `kill_all`)
/// that says `Successfully killed`. NEGATIVE CONTROLS: a kill of another, a
/// kill with no result row, and an idle row of another end nothing.
#[test]
fn a_subagent_holds_the_restart_until_idle_or_killed() {
    let spawn = spawned(SUB, AT);
    assert_eq!(
        open_work(&log(std::slice::from_ref(&spawn)), read_from()),
        Err(Open::Subagent)
    );
    assert_eq!(
        open_work(&log(&[spawn.clone(), idled(SUB, AT)]), read_from()),
        Ok(())
    );
    assert_eq!(
        open_work(&log(&[spawn.clone(), idled(SUB2, AT)]), read_from()),
        Err(Open::Subagent)
    );
    let kill = |ids: &str| {
        call(
            "manage_subagents",
            &[("Action", "\"kill\""), ("ConversationIds", ids)],
            AT,
        )
    };
    let named = log(&[
        spawn.clone(),
        kill(&format!("[\"{SUB}\"]")),
        killed_subagents(AT),
    ]);
    assert_eq!(open_work(&named, read_from()), Ok(()));
    let another = log(&[
        spawn.clone(),
        kill(&format!("[\"{SUB2}\"]")),
        killed_subagents(AT),
    ]);
    assert_eq!(open_work(&another, read_from()), Err(Open::Subagent));
    let unanswered = log(&[spawn.clone(), kill(&format!("[\"{SUB}\"]"))]);
    assert_eq!(open_work(&unanswered, read_from()), Err(Open::Subagent));
    let all = log(&[
        spawn.clone(),
        spawned(SUB2, AT),
        call("manage_subagents", &[("Action", "\"kill_all\"")], AT),
        killed_subagents(AT),
    ]);
    assert_eq!(open_work(&all, read_from()), Ok(()));
}

/// THE WINDOW: rows of an earlier process (a task its SIGTERM killed keeps
/// its `RUNNING` row with no end, measured) are not this one's; a row inside
/// the slack is. A torn line and a row with no time fail closed; no start
/// read proves nothing, nor does a tail that does not reach back to the
/// start.
#[test]
fn the_transcript_window_is_this_process_and_fails_closed() {
    let killed_before = log(&[
        task_started(23, "sleep 400", BEFORE),
        spawned(SUB, BEFORE),
        turn(AT),
    ]);
    assert_eq!(quiet_transcript(&killed_before, false, START_MS), Ok(()));
    let slack_at = "2026-10-18T18:43:19Z";
    assert!(super::super::upgrade_models::parse_utc(slack_at).unwrap() >= read_from());
    let within = log(&[task_started(23, "sleep 400", slack_at)]);
    assert_eq!(quiet_transcript(&within, false, START_MS), Err(Open::Task));
    let torn = format!("{}\n{{\"step_index\":5,\"sou", turn(AT));
    assert_eq!(open_work(&torn, read_from()), Err(Open::Unreadable));
    let timeless =
        r#"{"step_index":1,"source":"MODEL","type":"GENERIC","status":"RUNNING","content":"x"}"#;
    assert_eq!(open_work(timeless, read_from()), Err(Open::Unreadable));
    assert_eq!(quiet_transcript(&turn(AT), false, 0), Err(Open::Unproved));
    assert_eq!(
        quiet_transcript(&turn(AT), true, START_MS),
        Err(Open::Unproved),
        "a tail whose first row is this process's may have cut its start"
    );
    let reaches = log(&[turn(BEFORE), turn(AT)]);
    assert_eq!(quiet_transcript(&reaches, true, START_MS), Ok(()));
}

/// G-A1 AND G-A2(c), THE SUMMARY: the conversation's row idle with nothing
/// not fully idle and its subagents' rows idle (a killed one's too) lets it
/// go; its row `RUNNING` (a turn, and a turn's end a moment after the screen
/// reads it over) holds it as a turn, `not_fully_idle` as background work, a
/// subagent's row `RUNNING` as a subagent. NEGATIVE CONTROLS: no row of its
/// own proves nothing, and an answer of another shape, a row of another
/// conversation or a flag of another spelling is no answer.
#[test]
fn the_summary_row_reads_the_turn_and_the_work() {
    let rows = |lines: &[&str]| summary_rows(CONV, &lines.join("\n"));
    let idle = format!("{CONV}|{STATUS_IDLE}|0|");
    let killed = format!("{SUB}|{STATUS_IDLE}|0|{CONV}");
    assert_eq!(summary_gate(&rows(&[&idle, &killed])), Ok(()));
    let running = format!("{CONV}|CASCADE_RUN_STATUS_RUNNING|1|");
    assert_eq!(summary_gate(&rows(&[&running])), Err("turn-open"));
    let nfi = format!("{CONV}|{STATUS_IDLE}|1|");
    assert_eq!(summary_gate(&rows(&[&nfi])), Err("background-work"));
    let sub = format!("{SUB}|CASCADE_RUN_STATUS_RUNNING|1|{CONV}");
    assert_eq!(summary_gate(&rows(&[&idle, &sub])), Err("subagent-working"));
    assert_eq!(summary_gate(&rows(&[&killed])), Err("summary-unproved"));
    assert_eq!(summary_gate(&rows(&[])), Err("summary-unproved"));
    for bad in [
        format!("{CONV}|{STATUS_IDLE}|0"),
        format!("{SUB2}|{STATUS_IDLE}|0|{SUB}"),
        format!("{CONV}|{STATUS_IDLE}|maybe|"),
    ] {
        assert_eq!(
            summary_gate(&rows(&[&idle, &bad])),
            Err("summary-unreadable"),
            "{bad}"
        );
    }
    assert_eq!(
        rows(&[&format!("{CONV}|{STATUS_IDLE}|false|")]),
        Ok(Some(Summary {
            status: STATUS_IDLE.to_string(),
            not_fully_idle: false,
            children_busy: 0,
        }))
    );
}

/// G-A2(e), THE WAKES: a waking message (HIGH, or a priority not known)
/// `read.json` does not mark read holds the restart, and so does one marked
/// read within the settle of its writing; a LOW one never (a cancelled
/// task's end and the restart notice wait for the person's next message:
/// measured, unread for good). NEGATIVE CONTROLS: an old message marked read
/// holds nothing; a message or `read.json` that does not parse is no
/// answer.
#[test]
fn a_waking_message_unread_or_just_written_holds_the_restart() {
    let now = 1_792_349_400_000;
    let msg = |id: &str, priority: &str, ago_ms: u64| Message {
        id: id.to_string(),
        body: format!(
            r#"{{"id":"{id}", "recipient":"{CONV}", "sender":"{CONV}/task-4", "priority":"{priority}", "timestamp":"2026-10-18T18:53:20.270650Z", "content":"Task id finished"}}"#
        ),
        written_ms: now - ago_ms,
    };
    let old_high = msg("a48339bf", "MESSAGE_PRIORITY_HIGH", 60_000);
    let read = r#"{"a48339bf":true}"#;
    assert_eq!(
        wakes(Some(read), std::slice::from_ref(&old_high), now),
        Ok(0)
    );
    assert_eq!(
        wakes(Some("{}"), std::slice::from_ref(&old_high), now),
        Ok(1)
    );
    assert_eq!(wakes(None, std::slice::from_ref(&old_high), now), Ok(1));
    let fresh = msg("a48339bf", "MESSAGE_PRIORITY_HIGH", 12);
    assert_eq!(wakes(Some(read), &[fresh], now), Ok(1), "just written");
    let low = msg("4a1dded3", "MESSAGE_PRIORITY_LOW", 1);
    assert_eq!(wakes(Some("{}"), &[low], now), Ok(0));
    let unknown = msg("b3891043", "MESSAGE_PRIORITY_URGENT", 60_000);
    assert_eq!(wakes(Some("{}"), &[unknown], now), Ok(1));
    assert_eq!(wakes(None, &[], now), Ok(0));
    let torn = Message {
        body: "{\"id\":".to_string(),
        ..old_high.clone()
    };
    assert_eq!(wakes(Some(read), &[torn], now), Err(Unreadable));
    assert_eq!(wakes(Some("{"), &[old_high], now), Err(Unreadable));
}

/// THE ORDER: a newer `x.y.z` build moves, the same or an older never, nor
/// across grammars; a record whose directory is another build is no version.
#[test]
fn the_order_reads_atpkgs_plain_versions() {
    assert!(moves_onto("1.4.0", "1.4.1"));
    assert!(moves_onto("1.4.9", "1.4.10"));
    assert!(!moves_onto("1.4.0", "1.4.0"));
    assert!(!moves_onto("1.4.1", "1.4.0"));
    assert!(!moves_onto("1.4.0", "1.4.1-R1"));
    assert!(!moves_onto("1.4.0", "x"));
    assert!(build_version("1.4.0", Some("1000001000004000000")).is_some());
    assert!(build_version("1.4.1", Some("1000001000004000000")).is_none());
}

/// THE GATE'S ORDER, one word each, and `Ok` only where every witness says
/// the agy is idle with nothing running.
#[test]
fn the_gate_names_the_first_thing_that_holds_it() {
    let open = Look {
        held: false,
        shown: None,
        idle: true,
        composer_empty: true,
        summary: Ok(Some(Summary {
            status: STATUS_IDLE.to_string(),
            not_fully_idle: false,
            children_busy: 0,
        })),
        transcript: Ok(()),
        children: Some(0),
        wakes: Some(0),
        shared: Some(0),
    };
    assert_eq!(gate(&open), Ok(()));
    let cases: Vec<(Look, &str)> = vec![
        (
            Look {
                held: true,
                ..open.clone()
            },
            "held",
        ),
        (
            Look {
                shown: Some("a background task running"),
                idle: false,
                ..open.clone()
            },
            "background-work",
        ),
        (
            Look {
                idle: false,
                ..open.clone()
            },
            "screen-not-idle",
        ),
        (
            Look {
                composer_empty: false,
                ..open.clone()
            },
            "draft",
        ),
        (
            Look {
                summary: Err(Unreadable),
                ..open.clone()
            },
            "summary-unreadable",
        ),
        (
            Look {
                transcript: Err(Open::Subagent),
                ..open.clone()
            },
            "subagent-working",
        ),
        (
            Look {
                children: None,
                ..open.clone()
            },
            "process-table",
        ),
        (
            Look {
                children: Some(1),
                ..open.clone()
            },
            "child-process",
        ),
        (
            Look {
                shared: Some(1),
                ..open.clone()
            },
            "conversation-shared",
        ),
        (
            Look {
                shared: None,
                ..open.clone()
            },
            "conversation-shared",
        ),
        (
            Look {
                wakes: None,
                ..open.clone()
            },
            "messages-unreadable",
        ),
        (
            Look {
                wakes: Some(1),
                ..open.clone()
            },
            "queued-work",
        ),
    ];
    for (look, word) in cases {
        assert_eq!(gate(&look), Err(word), "{look:?}");
    }
}

/// G-A4, A LAUNCH THAT SET ITS OWN ENVIRONMENT: a variable assigned ahead of
/// agy on its own line (quoted, behind a `cd … &&`, through `env`, by a
/// path, or out of a `$(…)`, `${…}` or backtick substitution), or an `env`
/// that unsets one, is named — the relaunch line would lose it. NEGATIVE
/// CONTROLS: a bare launch, the managed twin by its path, `HOME` alone (the
/// relaunch carries it, so the line a restart typed is never refused next
/// time), an `export` of its own command, a variable set for another command
/// of the line, `env` (or `env --`) with nothing changed, a quoted separator
/// kept in its word, and a line naming no agy.
#[test]
fn a_launch_that_set_its_own_environment_is_named() {
    for (line, want) in [
        ("GEMINI_API_KEY=k agy", "GEMINI_API_KEY"),
        (
            "GEMINI_API_KEY='a b' GOOGLE_GEMINI_BASE_URL=https://proxy agy -c",
            "GEMINI_API_KEY",
        ),
        ("cd ~/p && HTTPS_PROXY=http://p:1 agy", "HTTPS_PROXY"),
        ("env GEMINI_API_KEY=k agy", "GEMINI_API_KEY"),
        ("HOME=/h X=1 '/p/agents/agy' --effort high", "X"),
        ("X='a;b' agy", "X"),
        ("env -u GOOGLE_API_KEY agy", "env -u"),
        ("FOO=1 nohup /opt/bin/agy", "FOO"),
        ("GEMINI_API_KEY=${WORK_KEY} agy", "GEMINI_API_KEY"),
        (
            "GEMINI_API_KEY=$(security find-generic-password -s gemini -w) agy",
            "GEMINI_API_KEY",
        ),
        (
            "HTTPS_PROXY=${CORP_PROXY:-http://p:1} agy -c",
            "HTTPS_PROXY",
        ),
        ("K=$( (cat ~/.k) ) agy", "K"),
        ("K=$((1 + 2)) agy", "K"),
        ("K=\"$(cat \")\")\" agy", "K"),
        ("K=`cat ~/.k` agy", "K"),
    ] {
        assert_eq!(inline_env(line).as_deref(), Some(want), "{line}");
    }
    for line in [
        "agy",
        "agy --effort high",
        "'/p/agents/agy' '--conversation=a1047651-e950-4c37-9043-83382fad0eb9'",
        "HOME='/s/agy-home' '/p/agents/agy' '--conversation=c1'",
        " ( cd -- '/w/proj' && exec '/p/agents/agy' '--conversation=c1' )",
        "export GEMINI_API_KEY=k; agy",
        "X=1 make; agy",
        "env agy",
        "env -- agy",
        "X=$(make) ; agy",
        "echo 'X=1 ; agy'",
        "sh agy.sh",
        "X=1 agyk",
        "",
    ] {
        assert_eq!(inline_env(line), None, "{line}");
    }
}
