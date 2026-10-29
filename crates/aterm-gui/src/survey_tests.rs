// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
use super::*;
use aterm_agent::harness::excerpt::Prompt;
use aterm_agent::harness::footer::FooterFacts;

fn fields(row: &str) -> std::collections::HashMap<String, String> {
    row.split_whitespace()
        .skip(1)
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.to_owned(), aterm_codec::pct::decode(v)))
        .collect()
}

fn here() -> Here {
    Here {
        host: Some("m7.local".to_owned()),
        instances: 1,
        drive: false,
        facts: None,
    }
}

fn ageless(reply: &str) -> String {
    reply
        .split('\n')
        .map(|line| {
            line.split(' ')
                .map(|kv| match kv.split_once('=') {
                    Some((key, _)) if key.ends_with("_ms") => format!("{key}=*"),
                    _ => kv.to_owned(),
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn prompt(text: &str) -> Prompt {
    Prompt {
        text: text.to_owned(),
        key: excerpt::prompt_key(text),
    }
}

fn turn(id: u64, text: &str, by: crate::turn_ledger::TurnBy) -> crate::turn_ledger::TurnRecord {
    crate::turn_ledger::TurnRecord {
        id,
        started_ms: 0,
        dur_ms: 1,
        submitted: true,
        status: "settled",
        text: text.into(),
        screen_hash: 0,
        seq: 0,
        arch: Default::default(),
        by,
        carried: false,
    }
}

fn claude_session(h: &SessionHandle) {
    const PGID: i32 = 4242;
    let mut tl = h.ctx.timeline.lock().unwrap();
    tl.note_foreground_group(PGID);
    tl.set_program(PGID, Some("claude".into()));
    tl.publish_agent(
        "busy",
        None,
        None,
        Some(aterm_phase::Program::Claude),
        crate::session_timeline::AgentStamp {
            generation: crate::control::ScreenGen { epoch: 1, seq: 7 },
            fp: 0x5eed,
        },
    );
    tl.set_gauges(crate::session_timeline::AgentGauges {
        ctx_pct: Some(12),
        shells: Some(2),
    });
    tl.set_claude_footer(
        PGID,
        None,
        Some(FooterFacts {
            model: Some("Opus 5.5".into()),
            effort: Some("xhigh".into()),
            path: Some("~/aterm".into()),
            branch: Some("mesh/sv".into()),
            ..FooterFacts::default()
        }),
    );
    tl.set_claude_excerpts(
        PGID,
        Some(ClaudeExcerpts {
            excerpts: Excerpts {
                prompts: vec![prompt("keep going"), prompt("wire the survey")],
                said: Some("Running the tests now.".into()),
                said_end: Some("Running the tests now.".into()),
                doing: Some("Bash".into()),
            },
            task: None,
            transcript_len: None,
        }),
    );
}

#[test]
fn the_survey_answers_a_header_and_a_row_per_session() {
    let store = crate::session_store::new_store();
    assert!(
        survey_rows(&store, SurveyLevel::Content, usize::MAX, &here())
            .starts_with("OK 1\ninstance pid=")
    );
    let shell = crate::session_store::test_handle(3);
    let claude = crate::session_store::test_handle(5);
    claude_session(&claude);
    claude.ctx.turns.lock().unwrap().push(turn(
        1,
        "keep going",
        crate::turn_ledger::TurnBy::Harness,
    ));
    {
        let mut meta = claude.ctx.meta.lock().unwrap();
        meta.role = Some("worker".into());
    }
    store.write().unwrap().register(shell);
    store.write().unwrap().register(claude.clone());

    let out = survey_rows(&store, SurveyLevel::Content, usize::MAX, &here());
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "OK 3", "{out}");
    let head = fields(lines[1]);
    assert!(lines[1].starts_with("instance "), "{out}");
    assert_eq!(head["sessions"], "2");
    assert_eq!(head["level"], "content");
    assert_eq!(head["pid"], std::process::id().to_string());
    let (shell_row, claude_row) = (fields(lines[2]), fields(lines[3]));
    assert_eq!(shell_row["local"], "3", "local-id order: {out}");
    assert_eq!(shell_row["agent"], "-");
    assert_eq!(shell_row["prompt"], "-");
    assert_eq!(claude_row["local"], "5");
    assert_eq!(claude_row["sid"], claude.sid.as_str());
    for (key, want) in [
        ("program", "claude"),
        ("program_kind", "agent"),
        ("agent", "busy"),
        ("bg", "2"),
        ("ctx_left_pct", "12"),
        ("model", "Opus 5.5"),
        ("effort", "xhigh"),
        ("doing", "Bash"),
        ("role", "worker"),
        ("cwd", "~/aterm"),
        ("branch", "mesh/sv"),
        ("said", "Running the tests now."),
        ("prompt", "wire the survey"),
        ("task", "-"),
    ] {
        assert_eq!(claude_row[key], want, "{key}: {}", lines[3]);
    }
    assert!(
        claude_row["agent_ms"].parse::<u64>().is_ok(),
        "{}",
        lines[3]
    );

    let facts = survey_rows(&store, SurveyLevel::Facts, usize::MAX, &here());
    let row = facts.lines().nth(3).expect("the claude row");
    assert_eq!(fields(row)["doing"], "Bash");
    for key in [
        "title=", "cwd=", "branch=", "prompt=", "said=", "cmd=", "rows=",
    ] {
        assert!(!row.contains(&format!(" {key}")), "{key} at facts: {row}");
    }
    assert_eq!(
        ageless(&cmd_survey(
            &store,
            "facts",
            SurveyLevel::Content,
            usize::MAX,
            &here()
        )),
        ageless(&facts)
    );
    assert!(
        cmd_survey(
            &store,
            "everything",
            SurveyLevel::Content,
            usize::MAX,
            &here()
        )
        .starts_with("ERR usage: survey")
    );

    {
        let mut turns = claude.ctx.turns.lock().unwrap();
        *turns = crate::turn_ledger::TurnLedger::default();
        turns.push(turn(2, "keep going", crate::turn_ledger::TurnBy::Ctl));
    }
    let out = survey_rows(&store, SurveyLevel::Content, usize::MAX, &here());
    assert_eq!(fields(out.lines().nth(3).unwrap())["prompt"], "keep going");
}

#[test]
fn a_tasked_workers_prompt_is_its_task() {
    let store = crate::session_store::new_store();
    let worker = crate::session_store::test_handle(7);
    claude_session(&worker);
    {
        let mut tl = worker.ctx.timeline.lock().unwrap();
        let mut excerpts = tl.claude_excerpts().cloned().unwrap();
        excerpts.task = Some("Fix the flaky test in crates/aterm-ctl".into());
        tl.set_claude_excerpts(4242, Some(excerpts));
    }
    store.write().unwrap().register(worker.clone());
    let prompt_now = || {
        fields(
            survey_rows(&store, SurveyLevel::Content, usize::MAX, &here())
                .lines()
                .nth(2)
                .unwrap(),
        )["prompt"]
            .clone()
    };
    assert_eq!(
        prompt_now(),
        "keep going",
        "no task record: the transcript's"
    );
    {
        let mut tl = worker.ctx.timeline.lock().unwrap();
        let mark = crate::task_record::TaskMark::new(crate::task_record::TaskState::Working);
        tl.mark_task(&mark, false, std::time::Instant::now(), 1);
    }
    assert_eq!(prompt_now(), "Fix the flaky test in crates/aterm-ctl");
    let row = survey_rows(&store, SurveyLevel::Facts, usize::MAX, &here());
    let row = fields(row.lines().nth(2).unwrap());
    assert_eq!(row["task"], "working");
    assert_eq!(row["task_rev"], "1");
}

#[test]
fn a_finished_turns_said_is_sent_keeping_its_end() {
    let store = crate::session_store::new_store();
    let claude = crate::session_store::test_handle(5);
    claude_session(&claude);
    {
        let mut tl = claude.ctx.timeline.lock().unwrap();
        tl.set_claude_excerpts(
            4242,
            Some(ClaudeExcerpts {
                excerpts: Excerpts {
                    said: Some("Created notes.md on TCP slow start…".into()),
                    said_end: Some("…`wc -w notes.md` printed 142.".into()),
                    ..Excerpts::default()
                },
                task: None,
                transcript_len: None,
            }),
        );
        let working = crate::task_record::TaskMark::new(crate::task_record::TaskState::Working);
        tl.mark_task(&working, false, std::time::Instant::now(), 1);
    }
    store.write().unwrap().register(claude.clone());
    let said = || {
        let out = survey_rows(&store, SurveyLevel::Content, usize::MAX, &here());
        fields(out.lines().nth(2).expect("the claude row"))["said"].clone()
    };
    assert_eq!(
        said(),
        "Created notes.md on TCP slow start…",
        "a turn in flight"
    );
    {
        let mut tl = claude.ctx.timeline.lock().unwrap();
        let done = crate::task_record::TaskMark::new(crate::task_record::TaskState::Done);
        tl.mark_task(&done, false, std::time::Instant::now(), 2);
    }
    assert_eq!(said(), "…`wc -w notes.md` printed 142.", "its task done");

    let idle = crate::session_store::test_handle(6);
    claude_session(&idle);
    {
        let mut tl = idle.ctx.timeline.lock().unwrap();
        tl.set_claude_excerpts(
            4242,
            Some(ClaudeExcerpts {
                excerpts: Excerpts {
                    said: Some("head…".into()),
                    said_end: Some("…end".into()),
                    ..Excerpts::default()
                },
                task: None,
                transcript_len: None,
            }),
        );
        tl.publish_agent(
            "idle",
            None,
            None,
            Some(aterm_phase::Program::Claude),
            crate::session_timeline::AgentStamp {
                generation: crate::control::ScreenGen { epoch: 1, seq: 8 },
                fp: 0x5eef,
            },
        );
    }
    store.write().unwrap().register(idle);
    let out = survey_rows(&store, SurveyLevel::Content, usize::MAX, &here());
    let row = fields(out.lines().nth(3).expect("the idle row"));
    assert_eq!(
        (row["agent"].as_str(), row["said"].as_str()),
        ("idle", "…end")
    );
}

#[test]
fn harness_typed_prompts_are_passed_over_by_key() {
    let mut ledger = crate::turn_ledger::TurnLedger::default();
    ledger.push(turn(
        1,
        "keep  going\n",
        crate::turn_ledger::TurnBy::Harness,
    ));
    ledger.push(turn(
        2,
        "the done check",
        crate::turn_ledger::TurnBy::Harness,
    ));
    ledger.push(turn(
        3,
        "a manager's follow-up",
        crate::turn_ledger::TurnBy::Ctl,
    ));
    let keys = harness_keys(&ledger);
    assert_eq!(keys.len(), 2);
    let excerpts = Excerpts {
        prompts: vec![
            prompt("keep going"),
            prompt("the done check"),
            prompt("ship it"),
        ],
        ..Excerpts::default()
    };
    assert_eq!(newest_prompt(&excerpts, &keys).as_deref(), Some("ship it"));
    assert_eq!(newest_prompt(&excerpts, &[]).as_deref(), Some("keep going"));
    let only_harness = Excerpts {
        prompts: vec![prompt("keep going")],
        ..Excerpts::default()
    };
    assert_eq!(newest_prompt(&only_harness, &keys), None);
}

#[test]
fn fleet_codes_and_credentials_are_redacted() {
    assert_eq!(
        redact("join with ATF1-q7xk-29fa-pm3c@m7.local:7100 now"),
        "join with ATF1-[redacted] now".replace("ATF1-", "atf1-")
    );
    assert_eq!(redact("code:atf1-abcd"), "code:atf1-[redacted]");
    #[cfg(unix)]
    {
        let code = aterm_net::fleet_member::JoinCode {
            secret: [0x5a; aterm_net::fleet_member::SECRET_LEN],
            hint: None,
        }
        .text_at(("m7.local", 7100));
        let body = &code["atf1-".len()..];
        let groups = body.split('@').next().unwrap();
        for (typed, want) in [
            (
                format!("ran aterm fleet join {body} then"),
                "ran aterm fleet join atf1-[redacted] then",
            ),
            (
                format!("ran {}", body.to_ascii_uppercase()),
                "ran atf1-[redacted]",
            ),
            (
                format!("typed ATF1- {}.", groups.replace('-', " ")),
                "typed atf1-[redacted].",
            ),
        ] {
            assert_eq!(redact(&typed), want, "{typed}");
        }
        let (first, second) = groups.split_at(20);
        let rows = format!("$ aterm fleet join ATF1- {first}\n{second}@m7.local:7100\njoined");
        assert_eq!(
            aterm_codec::pct::decode(&excerpt_token(Some(&rows))),
            "$ aterm fleet join atf1-[redacted]\n\njoined"
        );
        let spaced = format!("ATF1- {}", groups.replace('-', " "));
        let room = excerpt::EXCERPT_BYTES - '…'.len_utf8();
        for shift in 1..spaced.len() {
            let lead = format!(
                "{} ",
                &"but ".repeat(100)[..room - "ran ".len() - shift - 1]
            );
            let said = excerpt::cap(&format!("ran {lead}{spaced} then"), excerpt::EXCERPT_BYTES);
            let shown = aterm_codec::pct::decode(&excerpt_token(Some(&said)));
            for group in groups.split('-') {
                assert!(
                    !shown.to_ascii_lowercase().contains(group),
                    "{shift}: {shown}"
                );
            }
            assert!(shown.starts_with("ran but but"), "{shown}");
        }
    }
    let key = format!("sk-proj-{}", "a1B2c3D4".repeat(4));
    assert_eq!(redact(&format!("use {key} here")), "use [redacted] here");
    assert_eq!(
        redact(concat!("see AKIA", "IOSFODNN7EXAMPLE.")),
        "see [redacted]."
    );
    assert_eq!(
        redact(concat!(
            "pushed with ghp_",
            "abcdefghijklmnopqrstuvwxyz0123456789."
        )),
        "pushed with [redacted]."
    );
    assert_eq!(redact(&format!("({key}),")), "([redacted]),");
    assert_eq!(
        redact("password: hunter2\nall good"),
        "[redacted: sensitive]\nall good"
    );
    assert_eq!(
        redact("Committed 3f2a9c1 on main"),
        "Committed 3f2a9c1 on main"
    );
    let sha = "e23254f84a67131e6cad7724c7f749073b190dd2";
    assert_eq!(redact(&format!("at {sha}.")), "at e23254f84a67….");
    assert_eq!(
        redact(&format!("{sha}..{sha} key={sha}")),
        "e23254f84a67…..e23254f84a67… key=e23254f84a67…"
    );
    let cwd = "~/Developer/aterm-mesh-fleet2/crates/aterm-gui/src/Tests2026x";
    assert_eq!(redact(cwd), cwd);
    assert_eq!(
        redact("/Users//Ayates2026/Developer/aterm-mesh-sv3/crates"),
        "/Users//Ayates2026/Developer/aterm-mesh-sv3/crates"
    );
    assert_eq!(
        redact(&format!("cat ~/keys/{key}/env")),
        "cat ~/keys/[redacted]/env"
    );
    let cd = "mkdir -p \"$HOME/Developer/aterm-mesh-wt-07-2/crates/aterm-net2026x\"";
    assert_eq!(redact(cd), cd);
    assert_eq!(
        redact(&format!("cat $HOME/keys/{key}/env")),
        "cat $HOME/keys/[redacted]/env"
    );
    assert_eq!(
        redact("Developer-aterm-mesh-fleet2-crates-aterm-gui-Tests2026x"),
        "[redacted]"
    );
    assert_eq!(redact(&"ab12".repeat(16)), "[redacted]");
    let upper = sha.to_ascii_uppercase();
    assert_eq!(
        redact(&format!("key {upper}.")),
        format!("key {}….", &upper[..12])
    );
    assert_eq!(
        redact(&format!("use {} now", "AB12CD34".repeat(4))),
        "use [redacted] now"
    );
    assert_eq!(redact("see RFC9110 and HTTP2"), "see RFC9110 and HTTP2");
    let short = "AB12CD34".repeat(3);
    assert_eq!(redact(&short), short);
    assert_eq!(
        redact(&format!("{{\"note\":\"{key}\"}}")),
        "{\"note\":\"[redacted]\"}"
    );
    let token = excerpt_token(Some(&format!("{} atf1-zzzz", "x".repeat(600))));
    let decoded = aterm_codec::pct::decode(&token);
    assert!(decoded.len() <= excerpt::EXCERPT_BYTES, "{}", decoded.len());
    assert!(!decoded.contains("zzzz"));
    assert_eq!(excerpt_token(Some("  ")), "-");
}

/// A FLEET KEY never leaves whole: the text of its key file (`cat
/// …/fleet.key`) is 40 lowercase hex digits, a commit id's shape, and only a
/// commit id's short form of it is quoted — in a line, a path, a title.
#[cfg(unix)]
#[test]
fn a_fleet_key_file_is_never_quoted_whole() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-survey-key")
        .tempdir()
        .unwrap();
    let secret = aterm_net::fleet_member::mint_secret().unwrap();
    aterm_net::fleet_member::hold_book(dir.path())
        .unwrap()
        .write_key(&secret)
        .unwrap();
    let text = std::fs::read_to_string(dir.path().join(aterm_net::fleet::KEY_FILE)).unwrap();
    let key = text.trim();
    assert_eq!(key.len(), 40, "{text:?}");
    let shown = [
        redact(&text),
        redact(&format!("$ cat ~/.local/state/aterm/fleet.key\n{text}$ ")),
        redact(&format!("cd /tmp/{key}/x && echo FLEET_KEY={key}")),
        aterm_codec::pct::decode(&excerpt_token(Some(key))),
    ];
    for shown in &shown {
        assert!(!shown.contains(key), "the key went out: {shown}");
        assert!(shown.contains(&key[..12]), "{shown}");
    }
    assert!(
        format!("cat fleet.key\n{text}").contains(key),
        "the raw text holds the key"
    );
}

#[test]
fn a_secret_a_soft_wrap_split_is_judged_whole() {
    let key = "deadbeef".repeat(5);
    let mut term = aterm_core::terminal::Terminal::new(6, 18);
    term.process(b"$ sleep 30 # password=hunter2\r\n$ cat fleet.key\r\n");
    term.process(key.as_bytes());
    assert!(
        term.row_continues_previous(1) && term.row_continues_previous(4),
        "precondition: both lines wrap"
    );
    let rows = last_rows(&term).expect("rows");
    assert_eq!(
        rows.lines().nth(2),
        Some(key.as_str()),
        "the key's line is joined from its rows: {rows:?}"
    );
    let shown = redact(&rows);
    assert!(!shown.contains("hunter2"), "{shown}");
    assert!(!shown.contains(&key), "{shown}");
    assert!(shown.contains(&format!("{}…", &key[..12])), "{shown}");
    assert!(shown.starts_with("[redacted: sensitive]\n"), "{shown}");

    let by_row: Vec<String> = (0..usize::from(term.rows()))
        .map(|r| crate::control::visible_row(&term, r))
        .collect();
    let by_row = redact(&by_row.join("\n"));
    assert!(by_row.contains("hunter2"), "{by_row}");
    assert!(!by_row.contains(&format!("{}…", &key[..12])), "{by_row}");
}

#[test]
fn a_secret_at_the_excerpt_cap_is_never_quoted_in_part() {
    let secret = "Qm7xT2vR".repeat(5);
    let lead = "word ".repeat(76);
    let text = format!("{lead}{secret} and more");
    assert!(
        lead.len() < excerpt::EXCERPT_BYTES && lead.len() + secret.len() > excerpt::EXCERPT_BYTES,
        "precondition: the secret straddles the cap"
    );
    let said = excerpt::cap(&text, excerpt::EXCERPT_BYTES);
    let shown = aterm_codec::pct::decode(&excerpt_token(Some(&said)));
    assert_eq!(shown, format!("{}…", lead.trim_end()));
    let ending = format!("more and {secret} {}", lead.trim_end());
    let said = excerpt::cap_end(&ending, excerpt::EXCERPT_BYTES);
    let shown = aterm_codec::pct::decode(&excerpt_token(Some(&said)));
    assert_eq!(shown, format!("…{}", lead.trim_end()));

    let root = aterm_tempfile::Builder::new()
        .prefix("aterm-survey-cap")
        .tempdir()
        .unwrap();
    let tasks = root.path().join("tasks");
    std::fs::create_dir_all(&tasks).unwrap();
    let task = tasks.join("s-1.task");
    std::fs::write(&task, &text).unwrap();
    assert_eq!(
        task_prompt_at(&task, &tasks),
        Some(format!("{lead}{REDACTED_WORD} and more"))
    );
    let blank = "\n".repeat(TASK_HEAD_BYTES as usize - 30);
    std::fs::write(&task, format!("{blank}see {secret}")).unwrap();
    assert_eq!(task_prompt_at(&task, &tasks).as_deref(), Some("see"));

    let sentence = "the quick brown fox jumps over the lazy dog ".repeat(12);
    let shown = aterm_codec::pct::decode(&excerpt_token(Some(&excerpt::cap(
        &sentence,
        excerpt::EXCERPT_BYTES,
    ))));
    let kept = shown.strip_suffix('…').expect("cut");
    assert!(
        sentence.starts_with(&format!("{kept} ")),
        "every word whole: {shown}"
    );
    assert!(
        kept.len() + "jumps ".len() + '…'.len_utf8() >= excerpt::EXCERPT_BYTES,
        "only the word the cut fell in goes: {shown}"
    );
}

#[test]
fn a_branch_is_kept_as_its_repository_names_it() {
    for branch in [
        "jsmith/JIRA-12345-Fix-the-login-redirect",
        "fix/password-reset",
        "feature/api-key-rotation",
        "mesh/fleet",
    ] {
        assert_eq!(
            aterm_codec::pct::decode(&branch_token(Some(branch))),
            branch,
            "{branch}"
        );
    }
    assert_eq!(
        redact("jsmith/JIRA-12345-Fix-the-login-redirect"),
        "[redacted]"
    );
    assert_eq!(
        aterm_codec::pct::decode(&branch_token(Some("join/atf1-q7xk-29fa"))),
        "join/atf1-[redacted]"
    );
    let long = branch_token(Some(&"b".repeat(1000)));
    assert!(aterm_codec::pct::decode(&long).len() <= excerpt::EXCERPT_BYTES);
    assert_eq!(branch_token(Some(" ")), "-");
}

#[test]
fn a_task_head_is_read_only_under_the_tasks_dir() {
    let root = aterm_tempfile::Builder::new()
        .prefix("aterm-survey-task")
        .tempdir()
        .unwrap();
    let tasks = root.path().join("tasks");
    std::fs::create_dir_all(&tasks).unwrap();
    let task = tasks.join("s-1.task");
    std::fs::write(&task, "Fix the footer.\n\nThen run the tests.\n").unwrap();
    assert_eq!(
        task_prompt_at(&task, &tasks).as_deref(),
        Some("Fix the footer. Then run the tests.")
    );
    let outside = root.path().join("elsewhere.task");
    std::fs::write(&outside, "not a task").unwrap();
    assert_eq!(task_prompt_at(&outside, &tasks), None, "outside");
    assert_eq!(
        task_prompt_at(&tasks.join("../elsewhere.task"), &tasks),
        None,
        "a `..` path"
    );
    #[cfg(unix)]
    {
        let link = tasks.join("link.task");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        assert_eq!(task_prompt_at(&link, &tasks), None, "a symlink");
    }
    #[cfg(windows)]
    {
        let text = task.to_string_lossy();
        let (drive, rooted) = text.split_at(2);
        assert!(drive.ends_with(':') && rooted.starts_with('\\'), "{text}");
        let unrooted = format!("{drive}{}", &rooted[1..]);
        assert_eq!(
            task_prompt_at(Path::new(&unrooted), &tasks),
            None,
            "{unrooted}"
        );
    }
    assert_eq!(task_prompt_at(&tasks, &tasks), None, "the dir itself");
    assert_eq!(
        task_prompt_of_env(&["HOME=/x".into()]),
        None,
        "no task file named"
    );
}

#[test]
fn a_remote_hold_names_the_node_that_holds_it() {
    use crate::fabric::{HoldIssuer, cmd_hold};
    let store = crate::session_store::new_store();
    let h = crate::session_store::test_handle(6);
    let sid = h.sid.as_str().to_owned();
    store.write().unwrap().register(h);
    let row = |store: &Store| {
        let facts = survey_rows(store, SurveyLevel::Facts, usize::MAX, &here());
        fields(facts.lines().nth(2).expect("a session row"))
    };
    assert_eq!(
        cmd_hold(&store, &format!("{sid} on"), HoldIssuer::Owner),
        "OK hold=1\n"
    );
    assert_eq!(row(&store)["held_from"], "-", "a local hold names no node");
    let node = "0a".repeat(16);
    {
        let _driving = crate::fabric::DrivingAs::enter(crate::fabric::Driver {
            node: node.clone(),
            host: "mac-0a".to_owned(),
        });
        assert_eq!(
            cmd_hold(&store, &format!("{sid} on"), HoldIssuer::Remote),
            "OK hold=1\n"
        );
    }
    assert_eq!(row(&store)["held_from"], node);
    {
        let _driving = crate::fabric::DrivingAs::enter(crate::fabric::Driver {
            node: node.clone(),
            host: "mac-0a".to_owned(),
        });
        let _ = cmd_hold(&store, &format!("{sid} off"), HoldIssuer::Remote);
    }
    assert_eq!(row(&store)["held_from"], "-", "handed back");
}

#[test]
fn a_session_that_rang_says_so_on_its_row_and_a_quiet_one_does_not() {
    use crate::bell_ledger::{BellSound, Rang};
    let store = crate::session_store::new_store();
    let rang = crate::session_store::test_handle(1);
    let quiet = crate::session_store::test_handle(2);
    let at = aterm_time::unix_millis().unwrap_or(0);
    for sound in [BellSound::Sounded, BellSound::Limited] {
        rang.ctx.bells.ring(Rang {
            at_unix_ms: at,
            program: Some("zsh".into()),
            sound,
        });
    }
    store.write().unwrap().register(rang);
    store.write().unwrap().register(quiet);
    for level in [SurveyLevel::Facts, SurveyLevel::Content] {
        let reply = survey_rows(&store, level, usize::MAX, &here());
        let rows: Vec<_> = reply.lines().skip(2).map(fields).collect();
        assert_eq!(rows.len(), 2, "{reply}");
        assert_eq!(rows[0]["bells"], "2", "{reply}");
        assert_eq!(rows[0]["bells_sounded"], "1", "{reply}");
        let last: Vec<Vec<&str>> = rows[0]["last_bell"]
            .split(',')
            .map(|entry| entry.split('/').skip(1).collect())
            .collect();
        assert_eq!(last, [["limited", "zsh"], ["sounded", "zsh"]], "{reply}");
        assert_eq!(
            (
                rows[1]["bells"].as_str(),
                rows[1]["bells_sounded"].as_str(),
                rows[1]["last_bell"].as_str()
            ),
            ("0", "0", "-"),
            "{reply}"
        );
    }
}

#[test]
fn each_level_emits_exactly_its_fields() {
    const FACTS: &[&str] = &[
        "local",
        "sid",
        "state",
        "program",
        "program_kind",
        "agent",
        "agent_detail",
        "agent_ms",
        "human_ms",
        "bg",
        "task",
        "task_rev",
        "task_from",
        "task_ms",
        "model",
        "effort",
        "ctx_left_pct",
        "usage_5h_pct",
        "usage_7d_pct",
        "account",
        "detail",
        "doing",
        "role",
        "supervisor",
        "attention",
        "goal",
        "needs",
        "needs_by",
        "hold_by",
        "held_from",
        "typed_by",
        "typed_ms",
        "bells",
        "bells_sounded",
        "last_bell",
    ];
    const CONTENT: &[&str] = &[
        "user_title",
        "title",
        "cwd",
        "branch",
        "prompt",
        "said",
        "cmd",
        "rows",
    ];
    let keys = |row: &str| -> Vec<String> {
        row.split(' ')
            .skip(1)
            .filter_map(|kv| kv.split_once('=').map(|(k, _)| k.to_owned()))
            .collect()
    };
    let store = crate::session_store::new_store();
    let claude = crate::session_store::test_handle(5);
    claude_session(&claude);
    {
        let mut meta = claude.ctx.meta.lock().unwrap();
        meta.attention = Some("needs a decision on the API".into());
        meta.user_title = Some("customer acme renewal draft".into());
    }
    let _ = claude.ctx.typed.stamp(
        crate::typed_by::Caller::new(crate::typed_by::Who::Bridge),
        aterm_time::unix_millis().unwrap_or(0),
    );
    store.write().unwrap().register(claude);

    let facts = survey_rows(&store, SurveyLevel::Facts, usize::MAX, &here());
    let row = facts.lines().nth(2).unwrap();
    assert_eq!(keys(row), FACTS, "{row}");
    assert_eq!(fields(row)["attention"], "1", "a flag at facts: {row}");
    assert_eq!(fields(row)["typed_by"], "-", "no label at facts: {row}");
    assert_ne!(fields(row)["typed_ms"], "-", "{row}");
    assert!(!row.contains("decision"), "{row}");
    assert!(!row.contains("acme"), "the user title is text: {row}");
    let content = survey_rows(&store, SurveyLevel::Content, usize::MAX, &here());
    let row = content.lines().nth(2).unwrap();
    let mut both: Vec<&str> = FACTS.to_vec();
    both.extend(CONTENT);
    assert_eq!(keys(row), both, "{row}");
    assert_eq!(fields(row)["attention"], "needs a decision on the API");
    assert_eq!(fields(row)["user_title"], "customer acme renewal draft");
    assert_eq!(fields(row)["typed_by"], "the message bus");

    for (rest, most, level) in [
        ("", SurveyLevel::Facts, "facts"),
        ("content", SurveyLevel::Facts, "facts"),
        ("facts", SurveyLevel::Content, "facts"),
        ("", SurveyLevel::Content, "content"),
    ] {
        let reply = cmd_survey(&store, rest, most, usize::MAX, &here());
        assert_eq!(
            fields(reply.lines().nth(1).unwrap())["level"],
            level,
            "{rest:?} at most {most:?}"
        );
    }
}

#[test]
fn a_typist_named_by_a_tab_name_is_its_id_at_facts() {
    use crate::typed_by::{Caller, Named, Who};
    let store = crate::session_store::new_store();
    let typist = |words: &str, facts: &str| {
        Caller::named(
            Who::client_with(Some(900), 42, &|_| None, &|_| None, &|_| None),
            Named {
                words: words.to_string(),
                facts: Some(facts.to_string()),
                by_parent: true,
            },
        )
    };
    let now = aterm_time::unix_millis().unwrap_or(0);
    let pinned = crate::session_store::test_handle(5);
    let _ = pinned
        .ctx
        .typed
        .stamp(typist("acme-renewal", "s-0123"), now);
    let roled = crate::session_store::test_handle(6);
    let _ = roled.ctx.typed.stamp(typist("lab-sweep", "lab-sweep"), now);
    store.write().unwrap().register(pinned);
    store.write().unwrap().register(roled);
    let typed_by = |level: SurveyLevel| -> Vec<String> {
        survey_rows(&store, level, usize::MAX, &here())
            .lines()
            .filter(|l| l.starts_with("session "))
            .map(|row| fields(row)["typed_by"].clone())
            .collect()
    };
    let facts = typed_by(SurveyLevel::Facts);
    assert!(facts.contains(&"s-0123".to_string()), "{facts:?}");
    assert!(facts.contains(&"lab-sweep".to_string()), "{facts:?}");
    assert!(!facts.contains(&"acme-renewal".to_string()), "{facts:?}");
    let content = typed_by(SurveyLevel::Content);
    assert!(content.contains(&"acme-renewal".to_string()), "{content:?}");
}

#[test]
fn the_attention_excerpt_is_the_ask_the_need_names() {
    use aterm_types::needs::AttentionKind;
    let store = crate::session_store::new_store();
    let h = crate::session_store::test_handle(5);
    claude_session(&h);
    {
        let mut meta = h.ctx.meta.lock().unwrap();
        meta.set_bare_attention(Some("lunch".into()));
        meta.set_attention_kind(
            "sup",
            Some("limited: resets 9pm".into()),
            Some(AttentionKind::Wait),
        )
        .expect("a slot");
    }
    let publish = |h: &SessionHandle| {
        let attention = h.ctx.meta.lock().unwrap().attention_owners.kinded();
        let verdict = crate::needs::of(&crate::needs::NeedFacts {
            attention,
            ..Default::default()
        });
        h.ctx.timeline.lock().unwrap().set_need(verdict);
    };
    publish(&h);
    let ctx = h.ctx.clone();
    store.write().unwrap().register(h);
    let row = |level| {
        let reply = survey_rows(&store, level, usize::MAX, &here());
        fields(reply.lines().nth(2).unwrap())
    };
    let content = row(SurveyLevel::Content);
    assert_eq!(content["needs"], "ask");
    assert_eq!(content["attention"], "lunch");
    let facts = row(SurveyLevel::Facts);
    assert_eq!(facts["attention"], "1");
    ctx.meta.lock().unwrap().set_bare_attention(None);
    let handle = store
        .read()
        .unwrap()
        .by_local(5)
        .cloned()
        .expect("the session");
    publish(&handle);
    let content = row(SurveyLevel::Content);
    assert_eq!(content["needs"], "-");
    assert_eq!(content["attention"], "limited: resets 9pm");
    ctx.meta
        .lock()
        .unwrap()
        .set_bare_attention(Some("lunch".into()));
    ctx.timeline
        .lock()
        .unwrap()
        .set_need(crate::needs::Verdict::default());
    assert_eq!(row(SurveyLevel::Content)["attention"], "lunch");
    ctx.meta
        .lock()
        .unwrap()
        .set_attention_kind(
            "sup",
            Some("limited: resets 10pm".into()),
            Some(AttentionKind::Wait),
        )
        .expect("a slot");
    assert_eq!(
        row(SurveyLevel::Content)["attention"],
        "limited: resets 10pm"
    );
}

/// A HARNESS'S REPORT SAYS SO ON THE WIRE (§14.7): a `NEEDS YOU:` report its harness wrote is
/// `needs=ask needs_by=harness` on the facts row, so a sibling or a fleet peer lists it after the
/// work that has stopped; CONTROL: the same words as another owner's ask are `needs_by=-`.
#[test]
fn a_harness_report_says_needs_by_harness_and_a_direct_ask_does_not() {
    use aterm_types::needs::AttentionKind;
    let by = |owner: &str| {
        let store = crate::session_store::new_store();
        let h = crate::session_store::test_handle(5);
        claude_session(&h);
        h.ctx
            .meta
            .lock()
            .unwrap()
            .set_attention_kind(
                owner,
                Some("claude needs you: the release credential \u{2014} carrying on".into()),
                Some(AttentionKind::Ask),
            )
            .expect("a slot");
        let attention = h.ctx.meta.lock().unwrap().attention_owners.kinded();
        let verdict = crate::needs::of(&crate::needs::NeedFacts {
            attention,
            ..Default::default()
        });
        h.ctx.timeline.lock().unwrap().set_need(verdict);
        store.write().unwrap().register(h);
        let reply = survey_rows(&store, SurveyLevel::Facts, usize::MAX, &here());
        let row = fields(reply.lines().nth(2).unwrap());
        (row["needs"].clone(), row["needs_by"].clone())
    };
    assert_eq!(
        by(crate::harness_host::NEEDS_ATTENTION_OWNER),
        ("ask".to_string(), "harness".to_string())
    );
    assert_eq!(
        by("lab"),
        ("ask".to_string(), "-".to_string()),
        "CONTROL: a direct ask"
    );
}

#[test]
fn a_survey_past_its_cap_cuts_the_least_pressing_first() {
    let shell = |local: u64| {
        let h = crate::session_store::test_handle(local);
        h.term
            .lock()
            .unwrap()
            .process(format!("{}\r\n{}\r\n", "x".repeat(70), "y".repeat(70)).as_bytes());
        h
    };
    let store = crate::session_store::new_store();
    let claude = crate::session_store::test_handle(2);
    claude_session(&claude);
    for h in [shell(1), claude, shell(3)] {
        store.write().unwrap().register(h);
    }
    let rows = |reply: &str| -> Vec<std::collections::HashMap<String, String>> {
        reply.lines().skip(2).map(fields).collect()
    };
    let head = |reply: &str| fields(reply.lines().nth(1).unwrap());

    let full = survey_rows(&store, SurveyLevel::Content, usize::MAX, &here());
    assert_eq!(head(&full)["cut"], "0", "{full}");
    let roomy = survey_rows(&store, SurveyLevel::Content, full.len() + 64, &here());
    assert_eq!((head(&roomy)["cut"].as_str(), rows(&roomy).len()), ("0", 3));

    let max = full.len() - 1;
    let reply = survey_rows(&store, SurveyLevel::Content, max, &here());
    assert!(reply.len() <= max, "{} > {max}", reply.len());
    assert!(reply.starts_with("OK 4\n"), "{reply}");
    assert_eq!(head(&reply)["cut"], "1", "{reply}");
    let got = rows(&reply);
    assert!(
        got[0].contains_key("rows"),
        "the older shell keeps its rows"
    );
    assert_eq!(
        got[1]["said"], "Running the tests now.",
        "the agent keeps its words"
    );
    assert!(
        !got[2].contains_key("rows"),
        "the newest shell is cut: {reply}"
    );

    let reply = survey_rows(&store, SurveyLevel::Content, 1, &here());
    assert!(reply.starts_with("OK 1\n"), "{reply}");
    assert_eq!(head(&reply)["sessions"], "3");
    assert_eq!(head(&reply)["cut"], "0");

    let facts = survey_rows(&store, SurveyLevel::Facts, usize::MAX, &here());
    let max = facts.len() - 1;
    let reply = survey_rows(&store, SurveyLevel::Facts, max, &here());
    assert!(reply.len() <= max, "{} > {max}", reply.len());
    let locals: Vec<String> = rows(&reply).iter().map(|r| r["local"].clone()).collect();
    assert_eq!(locals, ["1", "2"], "{reply}");
    assert_eq!(head(&reply)["sessions"], "3");
}

#[test]
fn facts_carry_a_role_only_as_a_label() {
    let store = crate::session_store::new_store();
    let claude = crate::session_store::test_handle(5);
    claude_session(&claude);
    let key = format!("sk-proj-{}", "a1B2c3D4".repeat(4));
    claude.ctx.meta.lock().unwrap().role = Some(format!("agent:deploy {key}"));
    let plain = crate::session_store::test_handle(6);
    plain.ctx.meta.lock().unwrap().role = Some("agent:reviewer".into());
    let long = crate::session_store::test_handle(7);
    let slug = "agent:fixing-the-acme-billing-outage-today";
    long.ctx.meta.lock().unwrap().role = Some(slug.into());
    for h in [claude, plain, long] {
        store.write().unwrap().register(h);
    }
    let roles = |level| -> Vec<String> {
        let out = survey_rows(&store, level, usize::MAX, &here());
        assert!(!out.contains("a1B2c3D4"), "{level:?}: {out}");
        out.lines()
            .skip(2)
            .map(|row| fields(row)["role"].clone())
            .collect()
    };
    assert_eq!(roles(SurveyLevel::Facts), ["-", "agent:reviewer", "-"]);
    assert_eq!(
        roles(SurveyLevel::Content),
        ["agent:deploy [redacted]", "agent:reviewer", slug]
    );
}

#[test]
fn the_header_names_this_machine_and_its_instances() {
    let store = crate::session_store::new_store();
    let two = Here {
        host: Some("Andrews-MacBook-Pro.local".to_owned()),
        instances: 2,
        drive: false,
        facts: None,
    };
    let out = survey_rows(&store, SurveyLevel::Facts, usize::MAX, &two);
    let head = fields(out.lines().nth(1).unwrap());
    assert_eq!(head["host"], "Andrews-MacBook-Pro.local");
    assert_eq!(head["instances"], "2");
    let none = Here {
        host: None,
        instances: 1,
        drive: false,
        facts: None,
    };
    let out = survey_rows(&store, SurveyLevel::Facts, usize::MAX, &none);
    let head = fields(out.lines().nth(1).unwrap());
    assert_eq!(
        (head["host"].as_str(), head["instances"].as_str()),
        ("-", "1")
    );
}

#[test]
fn an_agent_with_no_excerpts_shows_its_last_rows() {
    let agy = crate::session_store::test_handle(4);
    {
        const PGID: i32 = 4343;
        let mut tl = agy.ctx.timeline.lock().unwrap();
        tl.note_foreground_group(PGID);
        tl.set_program(PGID, Some("agy".into()));
        tl.publish_agent(
            "idle",
            None,
            None,
            Some(aterm_phase::Program::Agy),
            crate::session_timeline::AgentStamp {
                generation: crate::control::ScreenGen { epoch: 1, seq: 3 },
                fp: 0x6e,
            },
        );
    }
    agy.term
        .lock()
        .unwrap()
        .process(b"The build is green.\r\nmy token is password=hunter2\r\n");
    let claude = crate::session_store::test_handle(5);
    claude_session(&claude);
    claude
        .term
        .lock()
        .unwrap()
        .process(b"a row on the Claude Code screen\r\n");
    let store = crate::session_store::new_store();
    store.write().unwrap().register(agy);
    store.write().unwrap().register(claude);

    let out = survey_rows(&store, SurveyLevel::Content, usize::MAX, &here());
    let lines: Vec<&str> = out.lines().collect();
    let (agy_row, claude_row) = (fields(lines[2]), fields(lines[3]));
    assert_eq!(agy_row["program"], "agy", "{out}");
    assert_eq!(agy_row["program_kind"], "agent", "{out}");
    assert!(
        agy_row["rows"].contains("The build is green."),
        "{}",
        lines[2]
    );
    assert!(!agy_row["rows"].contains("hunter2"), "{}", lines[2]);
    assert_eq!(claude_row["said"], "Running the tests now.", "{out}");
    assert_eq!(claude_row["rows"], "-", "{}", lines[3]);
}

#[test]
fn a_frame_the_program_has_not_finished_gives_no_rows() {
    let shell = crate::session_store::test_handle(6);
    shell
        .term
        .lock()
        .unwrap()
        .process(b"\x1b[?2026h\x1b[H\x1b[2Jbuilding 3 of 9\x1b[?2026l");
    shell
        .term
        .lock()
        .unwrap()
        .process(b"\x1b[?2026h\x1b[H\x1b[2Jbuilding 4 of 9");
    let store = crate::session_store::new_store();
    store.write().unwrap().register(shell.clone());

    let out = survey_rows(&store, SurveyLevel::Content, usize::MAX, &here());
    let row = out.lines().nth(2).expect("the session row");
    assert_eq!(fields(row)["rows"], "-", "{row}");

    shell
        .term
        .lock()
        .unwrap()
        .process(b"\r\nlinking\x1b[?2026l");
    let out = survey_rows(&store, SurveyLevel::Content, usize::MAX, &here());
    let row = out.lines().nth(2).expect("the session row");
    assert_eq!(fields(row)["rows"], "building 4 of 9\nlinking", "{row}");
}

#[test]
fn the_program_kind_names_a_shell() {
    assert_eq!(program_kind(true, Some("claude")), "agent");
    assert_eq!(program_kind(false, Some("-zsh")), "shell");
    assert_eq!(program_kind(false, Some("fish")), "shell");
    assert_eq!(program_kind(false, Some("nvim")), "program");
    assert_eq!(program_kind(false, None), "-");
}

const CODEX_PGID: i32 = 5151;

/// `h` with codex-alab in front: its reader names no state (`unknown`), as `CodexReader` does.
fn codex_session(h: &SessionHandle) {
    let mut tl = h.ctx.timeline.lock().unwrap();
    tl.note_foreground_group(CODEX_PGID);
    tl.set_program(CODEX_PGID, Some("codex".into()));
    tl.publish_agent(
        "unknown",
        None,
        None,
        Some(aterm_phase::Program::Codex),
        crate::session_timeline::AgentStamp {
            generation: crate::control::ScreenGen { epoch: 1, seq: 7 },
            fp: 0xc0de,
        },
    );
}

/// A row's usage: the context left Claude Code's own indicator shows wins; else its usage tap's
/// count (`context_window.remaining_percentage`, which Claude Code hands every status line,
/// where the indicator stands only near a compaction); else codex's rollout. The plan's windows
/// come from the tap, else the rollout.
#[test]
fn a_rows_usage_takes_the_screen_then_the_tap_then_the_rollout() {
    use aterm_agent::harness::tap::{Reading, Window};
    let window = |pct| {
        Some(Window {
            pct,
            resets_at: 1_793_130_600,
            pct_x10: Some(pct * 10),
        })
    };
    let tapped = Reading {
        at: 1_793_113_505,
        sid: "s-f0bcb335df22ac60a794".into(),
        pid: 43_105,
        session: None,
        login: None,
        home: None,
        api_ms: 5_206_957,
        five_hour: window(7),
        seven_day: window(1),
        warm: None,
        cache_expires_at: None,
        version: Some("2.1.311".into()),
        model: Some("claude-opus-5-5".into()),
        ctx_left: Some(64),
        dur_us: 0,
    };
    let codex = codex_survey().facts;
    let usage = |screen, tapped: Option<&Reading>, codex| Usage::of(screen, tapped, codex);
    assert_eq!(
        usage(None, Some(&tapped), None),
        Usage {
            ctx_left: Some(64),
            five_hour: Some(7),
            seven_day: Some(1),
        },
        "no indicator on the glass: the tap says it"
    );
    assert_eq!(
        usage(Some(9), Some(&tapped), None).ctx_left,
        Some(9),
        "the glass wins"
    );
    assert_eq!(
        usage(None, None, Some(&codex)),
        Usage {
            ctx_left: Some(67),
            five_hour: None,
            seven_day: Some(12),
        }
    );
    assert_eq!(usage(None, None, None), Usage::default());
}

fn codex_survey() -> CodexSurvey {
    CodexSurvey {
        facts: aterm_agent::harness::codex_facts::CodexFacts {
            excerpts: Excerpts {
                prompts: vec![prompt("port the parser")],
                said: Some("Reading the grammar.".into()),
                said_end: Some("Ported; the tests pass.".into()),
                doing: Some("exec_command".into()),
            },
            model: Some("gpt-5.5".into()),
            effort: Some("high".into()),
            ctx_left_pct: Some(67),
            usage_5h_pct: None,
            usage_7d_pct: Some(12),
            cwd: Some("/private/tmp/codex-work".into()),
        },
        branch: Some("feat/parser".into()),
        task: None,
        goal: None,
    }
}

fn shown(
    state: aterm_core::terminal::program_state::ProgramStateWord,
    kind: Option<aterm_core::terminal::program_state::ProgramBlockedKind>,
) -> Option<aterm_core::terminal::program_state::ProgramState> {
    Some(aterm_core::terminal::program_state::ProgramState {
        state,
        kind,
        app: aterm_core::terminal::program_state::ProgramApp::Codex,
    })
}

/// A codex row: its agent word is what codex-alab says of itself (OSC 7501) and its model,
/// effort, context, doing, prompt, said, cwd and branch are its rollout's (`CodexSurvey`), where a
/// Claude row reads its screen and footer; a Claude row with a shown state keeps its own word.
#[test]
fn a_codex_row_reads_its_own_state_and_its_rollouts_facts() {
    use aterm_core::terminal::program_state::{ProgramBlockedKind as K, ProgramStateWord as W};
    let store = crate::session_store::new_store();
    let codex = crate::session_store::test_handle(7);
    codex_session(&codex);
    store.write().unwrap().register(codex.clone());
    let row = || {
        let out = survey_rows(&store, SurveyLevel::Content, usize::MAX, &here());
        fields(out.lines().nth(2).expect("the codex row"))
    };
    let before = row();
    for key in [
        "model",
        "effort",
        "ctx_left_pct",
        "usage_5h_pct",
        "usage_7d_pct",
        "doing",
        "prompt",
        "said",
        "branch",
    ] {
        assert_eq!(before[key], "-", "{key} before any facts: {before:?}");
    }
    assert_eq!(before["agent"], "unknown");

    let mut tl = codex.ctx.timeline.lock().unwrap();
    assert!(tl.set_codex_survey(CODEX_PGID, codex_survey()));
    tl.note_program_state(shown(W::Working, None), 1);
    drop(tl);
    let working = row();
    for (key, want) in [
        ("agent", "busy"),
        ("program_kind", "agent"),
        ("model", "gpt-5.5"),
        ("effort", "high"),
        ("ctx_left_pct", "67"),
        ("usage_5h_pct", "-"),
        ("usage_7d_pct", "12"),
        ("doing", "exec_command"),
        ("prompt", "port the parser"),
        ("said", "Reading the grammar."),
        ("cwd", "/private/tmp/codex-work"),
        ("branch", "feat/parser"),
    ] {
        assert_eq!(working[key], want, "{key}: {working:?}");
    }

    for (state, kind, word) in [
        (W::Blocked, Some(K::Permission), "prompt"),
        (W::Blocked, None, "prompt"),
        (W::Blocked, Some(K::Question), "question"),
        (W::Blocked, Some(K::Auth), "wall:auth"),
        (W::Error, None, "idle"),
        (W::Done, None, "idle"),
    ] {
        codex
            .ctx
            .timeline
            .lock()
            .unwrap()
            .note_program_state(shown(state, kind), 1);
        assert_eq!(row()["agent"], word, "{state:?} {kind:?}");
    }
    assert_eq!(
        row()["said"],
        "Ported; the tests pass.",
        "a codex at rest says its turn's end"
    );

    let mut tl = codex.ctx.timeline.lock().unwrap();
    tl.note_program_state(None, 1);
    drop(tl);
    assert_eq!(
        row()["agent"],
        "unknown",
        "no report shown: its reader's word"
    );

    let mut tl = codex.ctx.timeline.lock().unwrap();
    assert!(
        !tl.set_codex_survey(CODEX_PGID + 1, codex_survey()),
        "another group's facts are not taken"
    );
    tl.note_foreground_group(CODEX_PGID + 1);
    tl.set_program(CODEX_PGID + 1, Some("zsh".into()));
    drop(tl);
    let shell = row();
    for key in [
        "model",
        "effort",
        "ctx_left_pct",
        "usage_7d_pct",
        "doing",
        "prompt",
        "said",
        "branch",
    ] {
        assert_eq!(
            shell[key], "-",
            "{key} once codex left the front: {shell:?}"
        );
    }

    let claude = crate::session_store::test_handle(9);
    claude_session(&claude);
    claude
        .ctx
        .timeline
        .lock()
        .unwrap()
        .note_program_state(shown(W::Idle, None), 1);
    store.write().unwrap().register(claude);
    let out = survey_rows(&store, SurveyLevel::Content, usize::MAX, &here());
    let claude_row = fields(out.lines().nth(3).expect("the claude row"));
    assert_eq!(
        claude_row["agent"], "busy",
        "a word its reader names stands"
    );
    assert_eq!(claude_row["model"], "Opus 5.5");
}

/// THE AGENT'S ACTIVE GOAL: a Claude row's from its footer facts, a codex row's from its rollout
/// tap's facts, the words at `content` and a flag at `facts`; a session with none says `-`.
#[test]
fn a_row_names_the_agents_active_goal() {
    let store = crate::session_store::new_store();
    let shell = crate::session_store::test_handle(3);
    let claude = crate::session_store::test_handle(5);
    let codex = crate::session_store::test_handle(7);
    claude_session(&claude);
    codex_session(&codex);
    {
        let mut tl = claude.ctx.timeline.lock().unwrap();
        let mut facts = tl.claude_footer().cloned().expect("the footer facts");
        facts.goal = Some("implement the aos aterm vision".into());
        assert!(tl.set_claude_footer(4242, None, Some(facts)));
    }
    assert!(codex.ctx.timeline.lock().unwrap().set_codex_survey(
        CODEX_PGID,
        CodexSurvey {
            goal: Some("port the lexer".into()),
            ..codex_survey()
        }
    ));
    for h in [&shell, &claude, &codex] {
        store.write().unwrap().register(h.clone());
    }
    let rows = |level| {
        let out = survey_rows(&store, level, usize::MAX, &here());
        out.lines().skip(2).map(fields).collect::<Vec<_>>()
    };
    let content = rows(SurveyLevel::Content);
    assert_eq!(content[0]["goal"], "-", "a shell has no goal");
    assert_eq!(content[1]["goal"], "implement the aos aterm vision");
    assert_eq!(content[2]["goal"], "port the lexer");
    let facts = rows(SurveyLevel::Facts);
    assert_eq!(
        [&facts[0]["goal"], &facts[1]["goal"], &facts[2]["goal"]],
        ["-", "1", "1"],
        "facts flag a goal, never its words"
    );
}

/// An instance's facts list each account row of its pools after its sessions, the header counting
/// them and naming each pool it could not read, with no email on the wire; each agent's row names
/// the account it runs on.
#[test]
fn an_instance_lists_its_account_rows_and_each_session_its_account() {
    use aterm_agent::survey_facts::{SurveyAccountFact, SwarmSurveyFacts};
    let row = |pool: &str, id: &str, condition: &str| SurveyAccountFact {
        pool: pool.to_owned(),
        id: id.to_owned(),
        condition: condition.to_owned(),
        enrolled: condition != "not-enrolled",
        usage_5h_pct: None,
        usage_7d_pct: None,
        week_reset: None,
        walled_until: None,
        last_used: None,
    };
    let store = crate::session_store::new_store();
    let claude = crate::session_store::test_handle(5);
    claude_session(&claude);
    {
        let mut tl = claude.ctx.timeline.lock().unwrap();
        let mut facts = tl.claude_footer().cloned().expect("the footer facts");
        facts.account = Some("work-2".into());
        assert!(tl.set_claude_footer(4242, None, Some(facts)));
    }
    let codex = crate::session_store::test_handle(6);
    codex_session(&codex);
    for h in [&claude, &codex] {
        store.write().unwrap().register(h.clone());
    }
    let facts = SwarmSurveyFacts {
        node: None,
        label: None,
        version: "0.146.0".to_owned(),
        window: true,
        window_last: None,
        sessions: 2,
        ssh_host_key: None,
        accounts: vec![
            SurveyAccountFact {
                usage_5h_pct: Some(10),
                usage_7d_pct: Some(42),
                last_used: Some(1_793_130_000),
                ..row("claude", "main", "usable")
            },
            SurveyAccountFact {
                walled_until: Some(1_793_140_000),
                ..row("claude", "work-2", "limited")
            },
            row("claude", "abcdef@example.com", "unseen"),
            row("codex", "codex-1", "not-enrolled"),
        ],
        accounts_unread: vec!["grok".to_owned()],
        session_accounts: std::collections::BTreeMap::from([(
            ("codex".to_owned(), codex.sid.as_str().to_owned()),
            "codex-1".to_owned(),
        )]),
        server_heard: Vec::new(),
    };
    let listing = Here {
        facts: Some(facts),
        ..here()
    };
    for level in [SurveyLevel::Facts, SurveyLevel::Content] {
        let out = survey_rows(&store, level, usize::MAX, &listing);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "OK 7", "a header, 2 sessions and 4 rows: {out}");
        let head = fields(lines[1]);
        assert_eq!([&head["accounts"], &head["accounts_unread"]], ["4", "grok"]);
        let rows: Vec<_> = lines[4..].iter().map(|line| fields(line)).collect();
        assert!(lines[4..].iter().all(|line| line.starts_with("account ")));
        assert_eq!(
            rows.iter()
                .map(|row| (row["pool"].as_str(), row["id"].as_str()))
                .collect::<Vec<_>>(),
            [
                ("claude", "main"),
                ("claude", "work-2"),
                ("claude", "ab\u{2026}@\u{2026}"),
                ("codex", "codex-1"),
            ]
        );
        for (key, want) in [
            ("enrolled", "1"),
            ("condition", "usable"),
            ("pct5", "10"),
            ("pct7", "42"),
            ("walled_until", "-"),
            ("last_used", "1793130000"),
        ] {
            assert_eq!(rows[0][key], want, "{key}: {}", lines[4]);
        }
        assert_eq!(
            [&rows[1]["condition"], &rows[1]["walled_until"]],
            ["limited", "1793140000"]
        );
        assert_eq!(
            [&rows[3]["enrolled"], &rows[3]["condition"]],
            ["0", "not-enrolled"]
        );
        assert!(
            !out.contains("example") && !out.contains("abcdef"),
            "no email on the wire: {out}"
        );
        let sessions: Vec<_> = lines[2..4].iter().map(|line| fields(line)).collect();
        assert_eq!(sessions[0]["account"], "work-2", "the footer's: {out}");
        assert_eq!(sessions[1]["account"], "codex-1", "its pool's record");
    }

    // A reply with no facts says nothing of its accounts, and lists no row.
    let out = survey_rows(&store, SurveyLevel::Facts, usize::MAX, &here());
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "OK 3", "{out}");
    assert!(!fields(lines[1]).contains_key("accounts"), "{out}");
    assert!(!out.contains("\naccount "), "{out}");
}

/// H2 RULE 5 (GATE3-1): a seamless update's restore notes the holder it finds in front as the
/// running block's job's exec with the identity of its image's file and the steers its kernel
/// record shows (here its `PATH`), before any read, so `claude` typed through its versions link
/// reads `detail=claude` in the survey and the sessions roster (and in `ctl status`, which reads
/// the same [`crate::session_status::executing_detail`]) right after the update. A note by the
/// image's canonical name alone confirms no typed word: the image stands in, never as `claude`.
/// NEGATIVE CONTROL (scratch): the restore noting `ExecNames::of_path` alone (as before H1/H2)
/// fails the first assertion with `2.1.271`.
#[cfg(unix)]
#[test]
fn a_restored_holder_typed_through_a_link_reads_its_typed_name() {
    use crate::command_ran::{ExecNames, Ran, RanFeed, Steering, Table, test_id};
    use crate::holder_image::HolderImage;
    const SHELL: i32 = 4000;
    const HOLDER: i32 = 4242;
    const VERSIONS: &str = "/u/jsmith/.local/share/claude/versions/2.1.271";
    fn in_front(_: i32) -> i32 {
        HOLDER
    }
    fn holder(pgid: i32, _: Option<&HolderImage>) -> HolderImage {
        HolderImage::of(
            pgid,
            Some(1),
            Some(VERSIONS.into()),
            atpkg::image::ImageBuild::unknown(None),
            0,
        )
    }
    fn kernel(_: i32, _: Steering<'_>) -> ExecNames {
        ExecNames {
            image: Some("2.1.271".into()),
            file: Some(test_id("2.1.271")),
            env_path: Some("/u/jsmith/.local/bin:/usr/bin".into()),
            ..ExecNames::default()
        }
    }
    fn unread(_: i32, _: Steering<'_>) -> ExecNames {
        ExecNames::default()
    }
    let restored = |read: fn(i32, Steering<'_>) -> ExecNames| {
        let ran = std::sync::Arc::new(Ran::with_files(
            Box::new(
                Table::default()
                    .program("/u/jsmith/.local/bin/claude", "2.1.271")
                    .program(VERSIONS, "2.1.271"),
            ),
            Some(std::path::Path::new("/u/jsmith").into()),
        ));
        let mut before = aterm_core::terminal::Terminal::new(24, 80);
        before.process(
            b"\x1b]7;file://localhost/w\x07\x1b]133;A\x07$ \x1b]133;B\x07claude --resume \
              QZP9WORD8\r\n\x1b]133;C\x07",
        );
        let term = std::sync::Arc::new(std::sync::Mutex::new(
            aterm_core::terminal::Terminal::from_checkpoint(&before.checkpoint()),
        ));
        let h = crate::session_store::test_handle_ran(7, term, std::sync::Arc::clone(&ran));
        crate::spawn::adopt_restored_holder(
            &h.term,
            &h.ctx.timeline,
            &RanFeed::reading(ran, SHELL, read),
            -1,
            SHELL,
            in_front,
            holder,
            |_| Vec::new(),
        );
        let status = crate::session_status::executing_detail(&h.term.lock().unwrap(), &h.ctx.ran);
        let store = crate::session_store::new_store();
        store.write().unwrap().register(h);
        let survey = survey_rows(&store, SurveyLevel::Facts, usize::MAX, &here());
        let row = survey.lines().nth(2).map(fields).unwrap_or_default();
        let roster = crate::control::cmd_sessions_store(&store, None);
        (status, row.get("detail").cloned(), roster)
    };
    let (status, survey, roster) = restored(kernel);
    assert_eq!(status.as_deref(), Some("claude"), "status: the typed word");
    assert_eq!(survey.as_deref(), Some("claude"), "survey");
    assert!(roster.contains(" detail=claude "), "sessions: {roster}");
    let (status, survey, roster) = restored(unread);
    assert_eq!(
        (status.as_deref(), survey.as_deref()),
        (Some("2.1.271"), Some("2.1.271")),
        "the canonical name alone confirms nothing typed; the image stands in"
    );
    for text in [roster.as_str(), survey.as_deref().unwrap_or_default()] {
        assert!(!text.contains("QZP9"), "no argument anywhere: {text}");
    }
}

#[test]
fn survey_rows_emits_c16_facts_and_round_trips_with_drive_isolation() {
    use aterm_agent::survey_facts::{ServerHeardFact, SurveyAccountFact, SwarmSurveyFacts};

    let store = crate::session_store::new_store();
    let shell = crate::session_store::test_handle(3);
    store.write().unwrap().register(shell);

    let facts = SwarmSurveyFacts {
        node: Some("0123456789abcdef0123456789abcdef".to_owned()),
        label: Some("m22".to_owned()),
        version: "0.146.0".to_owned(),
        window: true,
        window_last: Some(1_793_160_000),
        sessions: 1,
        ssh_host_key: Some("SHA256:abc123xyz".to_owned()),
        accounts: vec![SurveyAccountFact {
            pool: "claude".to_owned(),
            id: "c01".to_owned(),
            condition: "usable".to_owned(),
            enrolled: true,
            usage_5h_pct: Some(25),
            usage_7d_pct: Some(50),
            week_reset: Some(1_793_500_000),
            walled_until: None,
            last_used: Some(1_793_159_000),
        }],
        accounts_unread: Vec::new(),
        session_accounts: std::collections::BTreeMap::new(),
        server_heard: vec![ServerHeardFact {
            vault: "alab".to_owned(),
            subject: "age1ql3z7hjy54pw3hyww5ayyfg7zqgvc7w3j2elw8zmrj2kg5sfn9aqmcac8p".to_owned(),
            at: 1_793_159_999,
        }],
    };
    let non_drive = Here {
        host: Some("m22.local".to_owned()),
        instances: 1,
        drive: false,
        facts: Some(facts.clone()),
    };
    let reply = survey_rows(&store, SurveyLevel::Facts, usize::MAX, &non_drive);
    assert!(
        reply.starts_with("OK 4\ninstance "),
        "1 instance + 1 session + 1 account + 1 server_heard = 4 lines: {reply}"
    );
    assert!(!reply.lines().nth(1).unwrap().contains(" drive=1"));
    let parsed = SwarmSurveyFacts::from_survey_reply(&reply).expect("round-trip C16 survey facts");
    assert_eq!(parsed, facts);

    // Negative control for caller scope isolation: sharing the cached `facts` for a `drive=true`
    // caller emits `drive=1` on that reply without mutating `non_drive`.
    let with_drive = Here {
        drive: true,
        ..non_drive.clone()
    };
    let drive_reply = survey_rows(&store, SurveyLevel::Facts, usize::MAX, &with_drive);
    assert!(drive_reply.lines().nth(1).unwrap().contains(" drive=1"));
    let again = survey_rows(&store, SurveyLevel::Facts, usize::MAX, &non_drive);
    assert!(!again.lines().nth(1).unwrap().contains(" drive=1"));
}
