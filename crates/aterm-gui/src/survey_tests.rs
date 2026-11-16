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
        access: aterm_types::fleet_access::FleetAccess::ReadOnly,
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
                said_at_ms: None,
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
        "title=", "cwd=", "branch=", "prompt=", "said=", "cmd=", "rows=", "step=",
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
        tl.mark_task(&mark, &Default::default(), std::time::Instant::now(), 1);
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
        tl.mark_task(&working, &Default::default(), std::time::Instant::now(), 1);
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
        tl.mark_task(&done, &Default::default(), std::time::Instant::now(), 2);
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

/// A redacted line says nothing but that something was redacted when it is the whole-line
/// notice, or word notices alone: a bare token's, a bare join code's, with or without
/// punctuation around them.
///
/// CONTROL: a line that keeps a word beside its notice says something, and so does a line
/// with no letter or digit that holds no notice at all.
#[test]
fn only_redaction_is_a_line_of_notices_alone() {
    let hex = "ab12".repeat(16);
    let pat = format!("ghp_{}", "0123456789abcdefghij");
    for said in [
        "password hunter2".to_string(),
        hex.clone(),
        pat.clone(),
        format!("({pat})."),
        format!("{hex} {pat}"),
        "atf1-abcd".to_string(),
        format!("atf1-abcd: {hex}"),
    ] {
        let line = redact(&said);
        assert!(
            !line.contains(&hex) && !line.contains(&pat) && !line.contains("hunter2"),
            "{line}"
        );
        assert!(only_redaction(&line), "{said} -> {line}");
    }
    assert!(only_redaction(REDACTED_LINE) && only_redaction(REDACTED_WORD));
    for said in [
        format!("use {pat} here"),
        format!("deploy {hex}"),
        "join atf1-abcd".to_string(),
        "->".to_string(),
        "✅".to_string(),
        "plain words".to_string(),
        String::new(),
    ] {
        let line = redact(&said);
        assert!(!line.contains(&hex) && !line.contains(&pat), "{line}");
        assert!(!only_redaction(&line), "{said} -> {line}");
    }
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
        "wait",
        "wait_gib",
        "wait_ahead",
        "agent",
        "agent_detail",
        "agent_ms",
        "human_ms",
        "bg",
        "task",
        "task_rev",
        "task_from",
        "task_ms",
        "task_conclusion",
        "model",
        "effort",
        "ctx_left_pct",
        "usage_5h_pct",
        "usage_7d_pct",
        "account",
        "auth",
        "token",
        "upgrade",
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
        "request",
        "request_from",
        "request_why",
        "bells",
        "bells_sounded",
        "last_bell",
        "turn",
        "turn_ms",
        "turn_end_ms",
        "step_ms",
        "out_ms",
        "out_src",
        "tok_in",
        "tok_out",
        "tok_for_ms",
        "turns",
        "tools",
        "pinned",
        "opened_ms",
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
        "step",
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
        access: aterm_types::fleet_access::FleetAccess::ReadOnly,
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
        access: aterm_types::fleet_access::FleetAccess::ReadOnly,
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

/// The header says this Mac's fleet access to every reader, at facts as at content, and before
/// the `drive=1` a reader its grants let drive is told.
#[test]
fn the_header_names_this_machines_fleet_access() {
    use aterm_types::fleet_access::FleetAccess;
    let store = crate::session_store::new_store();
    for access in FleetAccess::ALL {
        for (level, drive) in [(SurveyLevel::Facts, false), (SurveyLevel::Content, true)] {
            let here = Here {
                host: None,
                instances: 1,
                access,
                drive,
                facts: None,
            };
            let out = survey_rows(&store, level, usize::MAX, &here);
            let head = out.lines().nth(1).unwrap();
            assert_eq!(fields(head)["access"], access.word(), "{head}");
            let tail = format!(
                " access={}{}",
                access.word(),
                if drive { " drive=1" } else { "" }
            );
            assert!(head.ends_with(&tail), "{head}");
        }
    }
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
        effort: None,
        ctx_left: Some(64),
        dur_us: 0,
        row: None,
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
                said_at_ms: None,
                doing: Some("exec_command".into()),
            },
            model: Some("gpt-5.5".into()),
            effort: Some("high".into()),
            ctx_left_pct: Some(67),
            usage_5h_pct: None,
            usage_7d_pct: Some(12),
            cwd: Some("/private/tmp/codex-work".into()),
            tok_in: None,
            tok_out: None,
        },
        branch: Some("feat/parser".into()),
        task: None,
        goal: None,
        thread: "019a0000-0000-7000-8000-00000000c0de".into(),
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

/// A CODEX ROW'S `bg=` IS ITS OWN RECORD'S COUNT: the background terminals codex-alab's session
/// record names (`backgroundTerminals`, which the rollout tap puts on its own slot,
/// [`crate::session_timeline::SessionTimeline::set_codex_terminals`]) while the group it was read
/// for is in front, with an identified reader or none; `-` where the record names none or another
/// program is in front. The count publishes no survey facts, so the row keeps its screen's `rows=`
/// and `cmd=`. A Claude row keeps its screen's count. A `gauges()` that preferred the claim to the
/// screen is an equivalent mutant: no codex screen draws a count aterm reads.
#[test]
fn a_codex_row_takes_bg_from_its_own_records_count() {
    let store = crate::session_store::new_store();
    let codex = crate::session_store::test_handle(7);
    codex_session(&codex);
    codex
        .term
        .lock()
        .unwrap()
        .process(b"a row on the codex screen\r\n");
    store.write().unwrap().register(codex.clone());
    let row = |n: usize| {
        let out = survey_rows(&store, SurveyLevel::Content, usize::MAX, &here());
        fields(out.lines().nth(n).expect("the row"))
    };
    let count = |count: Option<u32>| {
        assert!(
            codex
                .ctx
                .timeline
                .lock()
                .unwrap()
                .set_codex_terminals(CODEX_PGID, count),
            "codex is in front"
        );
    };

    let before = row(2);
    assert_eq!(before["bg"], "-", "nothing published: {before:?}");
    assert!(
        before["rows"].contains("a row on the codex screen"),
        "a codex row with no rollout reads its screen: {before:?}"
    );
    let (rows, cmd) = (before["rows"].clone(), before["cmd"].clone());

    count(Some(2));
    let counted = row(2);
    assert_eq!(counted["bg"], "2", "{counted:?}");
    assert_eq!(
        counted["rows"], rows,
        "a count is no survey fact: {counted:?}"
    );
    assert_eq!(counted["cmd"], cmd, "{counted:?}");
    count(Some(0));
    assert_eq!(row(2)["bg"], "0", "none running");
    count(None);
    assert_eq!(row(2)["bg"], "-", "an older codex-alab's record names none");

    count(Some(2));
    {
        let mut tl = codex.ctx.timeline.lock().unwrap();
        tl.note_foreground_group(CODEX_PGID + 1);
        tl.set_program(CODEX_PGID + 1, Some("codex".into()));
        tl.publish_agent(
            "unknown",
            None,
            None,
            Some(aterm_phase::Program::Codex),
            crate::session_timeline::AgentStamp {
                generation: crate::control::ScreenGen { epoch: 1, seq: 8 },
                fp: 0xc0df,
            },
        );
    }
    assert_eq!(
        row(2)["bg"],
        "-",
        "another identified agent in front: the count was read for another group"
    );

    let bare = crate::session_store::test_handle(8);
    {
        let mut tl = bare.ctx.timeline.lock().unwrap();
        tl.note_foreground_group(CODEX_PGID);
        tl.set_program(CODEX_PGID, Some("codex-alab".into()));
        assert!(tl.set_codex_terminals(CODEX_PGID, Some(2)));
    }
    store.write().unwrap().register(bare);
    let bare_row = row(3);
    assert_eq!(
        bare_row["program_kind"], "program",
        "no reader identified it: {bare_row:?}"
    );
    assert_eq!(
        bare_row["bg"], "2",
        "the claim stands without a reader: {bare_row:?}"
    );

    let claude = crate::session_store::test_handle(9);
    claude_session(&claude);
    store.write().unwrap().register(claude);
    assert_eq!(row(4)["bg"], "2", "a Claude row keeps its screen's count");
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
/// them and naming each pool it could not read, each row by its label and its address; each
/// agent's row names the account it runs on, by its address where its footer knows one.
/// NEGATIVE CONTROL: with `session_row` reading the footer's `account` alone, the Claude Code
/// session reads `work-2`, its row's label, where its login file names `unicorn@example.com`.
#[test]
fn an_instance_lists_its_account_rows_and_each_session_its_account() {
    use aterm_agent::survey_facts::{SurveyAccountFact, SwarmSurveyFacts};
    let row = |pool: &str, id: &str, condition: &str| SurveyAccountFact {
        pool: pool.to_owned(),
        id: id.to_owned(),
        email: None,
        condition: condition.to_owned(),
        enrolled: condition != "not-enrolled",
        usage_5h_pct: None,
        usage_7d_pct: None,
        week_reset: None,
        walled_until: None,
        last_used: None,
        read_at: None,
        launch: None,
        token: None,
    };
    let store = crate::session_store::new_store();
    let claude = crate::session_store::test_handle(5);
    claude_session(&claude);
    {
        let mut tl = claude.ctx.timeline.lock().unwrap();
        let mut facts = tl.claude_footer().cloned().expect("the footer facts");
        facts.account = Some("work-2".into());
        facts.account_email = Some("unicorn@example.com".into());
        assert!(tl.set_claude_footer(4242, None, Some(facts)));
    }
    let codex = crate::session_store::test_handle(6);
    codex_session(&codex);
    for h in [&claude, &codex] {
        store.write().unwrap().register(h.clone());
    }
    let facts = SwarmSurveyFacts {
        node: None,
        operation_lines: Vec::new(),
        label: None,
        roster_unread: false,
        version: "0.146.0".to_owned(),
        window: true,
        window_last: None,
        sessions: 2,
        ssh_host_key: None,
        moves: None,
        accounts: vec![
            SurveyAccountFact {
                usage_5h_pct: Some(10),
                usage_7d_pct: Some(42),
                last_used: Some(1_793_130_000),
                read_at: None,
                ..row("claude", "main", "usable")
            },
            SurveyAccountFact {
                walled_until: Some(1_793_140_000),
                ..row("claude", "work-2", "limited")
            },
            SurveyAccountFact {
                email: Some("abcdef@example.com".to_owned()),
                ..row("claude", "work-3", "unseen")
            },
            row("codex", "codex-1", "not-enrolled"),
        ],
        accounts_unread: vec!["grok".to_owned()],
        session_accounts: std::collections::BTreeMap::from([(
            ("codex".to_owned(), codex.sid.as_str().to_owned()),
            "codex-1".to_owned(),
        )]),
        server_heard: Vec::new(),
        home: None,
        home_dir: None,
        home_free_mb: None,
        home_total_mb: None,
        home_build: None,
        store_worst: None,
        machine: None,
        repos: None,
        update: None,
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
                ("claude", "work-3"),
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
        assert_eq!(
            rows.iter()
                .map(|row| row["email"].as_str())
                .collect::<Vec<_>>(),
            ["-", "-", "abcdef@example.com", "-"]
        );
        let sessions: Vec<_> = lines[2..4].iter().map(|line| fields(line)).collect();
        assert_eq!(
            sessions[0]["account"], "unicorn@example.com",
            "the footer's address: {out}"
        );
        assert_eq!(sessions[1]["account"], "codex-1", "its pool's record");
    }

    // A reply with no facts says nothing of its accounts, and lists no row.
    let out = survey_rows(&store, SurveyLevel::Facts, usize::MAX, &here());
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "OK 3", "{out}");
    assert!(!fields(lines[1]).contains_key("accounts"), "{out}");
    assert!(!out.contains("\naccount "), "{out}");
}

/// The shell, the holder (or the shell's child) and the file `claude` links to in the restores
/// below, and the wall ms the restored block's C is stamped at.
#[cfg(unix)]
const RESTORED_SHELL: i32 = 4000;
#[cfg(unix)]
const RESTORED_HOLDER: i32 = 4242;
#[cfg(unix)]
const RESTORED_VERSIONS: &str = "/u/jsmith/.local/share/claude/versions/2.1.271";
#[cfg(unix)]
const RESTORED_C_MS: u64 = 1_700_000_000_000;

/// The probes a restore below reads its session with: each process's names, the group in front,
/// each group leader's image, the shell's children, and each process's birth once it has exec'd
/// (a test feed's starts are wall-clock microseconds: [`crate::command_ran::RanFeed::reading`]).
#[cfg(unix)]
#[derive(Clone, Copy)]
struct RestoreProbes {
    read: fn(i32, crate::command_ran::Steering<'_>) -> crate::command_ran::ExecNames,
    in_front: fn(i32) -> i32,
    image: crate::holder_image::ReadImage,
    children: fn(i32) -> Vec<i32>,
    execd: fn(i32) -> Option<u64>,
}

/// What a seamless update's restore reads of a session whose restored block runs `typed` (a
/// planted argument after it; `None`: the session is at a prompt, no block runs), its shell
/// running jobs (`jobs`) or every command in its own group (pwsh), with `probes`: the detail
/// `ctl status` gives (the same [`crate::session_status::executing_detail`] fabric status
/// reads), the survey's and the sessions roster's, `-` for none, read right after
/// [`crate::spawn::adopt_restored_holder`], and how many execs the restore noted. The block's C
/// is stamped by hand at [`RESTORED_C_MS`], long before the restore. No reader carries the
/// argument.
#[cfg(unix)]
fn restored_reads(typed: Option<&str>, jobs: bool, probes: RestoreProbes) -> ([String; 3], usize) {
    use crate::command_ran::{Ran, RanFeed, Table, Window};
    let ran = std::sync::Arc::new(Ran::with_files(
        Box::new(
            Table::default()
                .program("/u/jsmith/.local/bin/claude", "2.1.271")
                .program(RESTORED_VERSIONS, "2.1.271"),
        ),
        Some(std::path::Path::new("/u/jsmith").into()),
    ));
    ran.runs_jobs(jobs);
    let mut before = aterm_core::terminal::Terminal::new(24, 80);
    let line = typed.map_or_else(String::new, |typed| {
        format!("{typed} --resume QZP9WORD8\r\n\x1b]133;C\x07")
    });
    before.process_at(
        format!("\x1b]7;file://localhost/w\x07\x1b]133;A\x07$ \x1b]133;B\x07{line}").as_bytes(),
        aterm_core::terminal::ClockReading {
            monotonic: aterm_core::terminal::ClockReading::now().monotonic,
            wall_ms: Some(RESTORED_C_MS),
        },
    );
    let term = std::sync::Arc::new(std::sync::Mutex::new(
        aterm_core::terminal::Terminal::from_checkpoint(&before.checkpoint()),
    ));
    let h = crate::session_store::test_handle_ran(7, term, std::sync::Arc::clone(&ran));
    crate::spawn::adopt_restored_holder(
        &h.term,
        &h.ctx.timeline,
        &RanFeed::reading(std::sync::Arc::clone(&ran), RESTORED_SHELL, probes.read),
        -1,
        RESTORED_SHELL,
        probes.in_front,
        probes.image,
        probes.children,
        probes.execd,
    );
    let noted = ran.within(Some(Window { from: 0, to: None })).len();
    let status = crate::session_status::executing_detail(&h.term.lock().unwrap(), &h.ctx.ran);
    let store = crate::session_store::new_store();
    store.write().unwrap().register(h);
    let survey = survey_rows(&store, SurveyLevel::Facts, usize::MAX, &here());
    let roster = crate::control::cmd_sessions_store(&store, None);
    for text in [survey.as_str(), roster.as_str()] {
        assert!(!text.contains("QZP9"), "no argument anywhere: {text}");
    }
    let detail = |line: &str| {
        line.split_whitespace()
            .find_map(|kv| kv.strip_prefix("detail="))
            .map(aterm_codec::pct::decode)
            .unwrap_or_default()
    };
    (
        [
            status.unwrap_or_else(|| "-".to_owned()),
            detail(survey.lines().nth(2).unwrap_or_default()),
            detail(&roster),
        ],
        noted,
    )
}

/// The same reading in status, the survey and the sessions roster.
#[cfg(unix)]
fn thrice(detail: &str) -> [String; 3] {
    [detail, detail, detail].map(str::to_owned)
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
    use crate::command_ran::{ExecNames, Steering, test_id};
    use crate::holder_image::HolderImage;
    fn in_front(_: i32) -> i32 {
        RESTORED_HOLDER
    }
    fn holder(pgid: i32, _: Option<&HolderImage>) -> HolderImage {
        HolderImage::of(
            pgid,
            Some(1),
            Some(RESTORED_VERSIONS.into()),
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
        let probes = RestoreProbes {
            read,
            in_front,
            image: holder,
            children: |_| Vec::new(),
            execd: |_| None,
        };
        restored_reads(Some("claude"), true, probes).0
    };
    assert_eq!(
        restored(kernel),
        thrice("claude"),
        "status, survey and sessions: the typed word"
    );
    assert_eq!(
        restored(unread),
        thrice("2.1.271"),
        "the canonical name alone confirms nothing typed; the image stands in"
    );
}

/// M2 (GATE4-1): a restored holder whose executable was deleted after it started is noted by the
/// exec name the kernel keeps alone. macOS's `proc_pidpath` names no path once the file is
/// unlinked (as `brew upgrade`'s cleanup does under a running `nvim`), so the image has none,
/// and the kernel keeps the name of the file it was exec'd from (`p_name`; here a program whose
/// own file is named `claude`); Linux's `/proc/<pid>/exe` names the path it had, marked
/// ` (deleted)`, which the image keeps unmarked and the read finds gone (here `nvim`). Either
/// way a block typed by that very name reads it in status, the survey and the sessions roster
/// right after the update; a block typed `QZP7M2A`, which no kernel name equals, reads `-`
/// (the name never stands in, nor the path the image kept); and a typed path to a file the read
/// claims the image maps reads `-` (a gone image confirms by a kernel name alone, never by a
/// file's identity). NEGATIVE CONTROLS (scratch): the restore returning when the image has no
/// path (as before M2) reads `-` for `claude`; the gone image's exec name noted as its file name
/// too stands in as `claude` for `QZP7M2A`; the read's file kept reads `claude` for the path;
/// a gone read of an image with a path noted by that path's name (as before the land) stands in
/// as `nvim` for `QZP7M2A`.
#[cfg(unix)]
#[test]
fn a_restored_holder_whose_file_is_gone_reads_the_name_the_kernel_keeps() {
    use crate::command_ran::{ExecNames, Steering, test_id};
    use crate::holder_image::HolderImage;
    fn in_front(_: i32) -> i32 {
        RESTORED_HOLDER
    }
    fn pathless(pgid: i32, _: Option<&HolderImage>) -> HolderImage {
        HolderImage::of(
            pgid,
            Some(1),
            None,
            atpkg::image::ImageBuild::unknown(None),
            0,
        )
    }
    fn unmarked(pgid: i32, _: Option<&HolderImage>) -> HolderImage {
        HolderImage::of(
            pgid,
            Some(1),
            Some("/opt/nvim/bin/nvim".into()),
            atpkg::image::ImageBuild::unknown(None),
            0,
        )
    }
    fn kernel(_: i32, _: Steering<'_>) -> ExecNames {
        ExecNames::gone(Some("claude".into()))
    }
    fn deleted(_: i32, _: Steering<'_>) -> ExecNames {
        ExecNames::gone(Some("nvim".into()))
    }
    fn claims_a_file(_: i32, _: Steering<'_>) -> ExecNames {
        ExecNames {
            exec: Some("QZP7M2B".into()),
            file: Some(test_id("2.1.271")),
            ..ExecNames::default()
        }
    }
    let restored = |typed: &str,
                    image: crate::holder_image::ReadImage,
                    read: fn(i32, Steering<'_>) -> ExecNames| {
        let probes = RestoreProbes {
            read,
            in_front,
            image,
            children: |_| Vec::new(),
            execd: |_| None,
        };
        restored_reads(Some(typed), true, probes).0
    };
    assert_eq!(
        [
            restored("claude", pathless, kernel),
            restored("QZP7M2A", pathless, kernel),
            restored("/u/jsmith/.local/bin/claude", pathless, claims_a_file),
            restored("nvim", unmarked, deleted),
            restored("QZP7M2A", unmarked, deleted),
        ],
        [
            thrice("claude"),
            thrice("-"),
            thrice("-"),
            thrice("nvim"),
            thrice("-"),
        ],
        "the kernel's name, by equality alone"
    );
}

/// M2 (GATE4-2): pwsh runs every command in its own group, so after a seamless update the
/// foreground group is the shell's and its running command is one of the shell's children. The
/// restore notes each live child that has exec'd since its fork with the identity a restored
/// holder's is noted with, placed by its birth: here `claude` through its versions link reads
/// `claude` in status, the survey and the sessions roster whether it started before the running
/// block or in it; for a block typed `QZP7M2A` (an alias, a function), a child started in the
/// block's window stands in as its program, `2.1.271`, as its exec heard live did, while one
/// started before it (a `Start-Process` an earlier block left running) never does, so that block
/// reads `-`; a fork that has not exec'd (it maps the shell's image) is never noted, so `pwsh`
/// typed reads `-`, while a nested `pwsh` that has exec'd reads `pwsh`; a child whose file is
/// gone reads the name the kernel keeps for a block typed by it and never stands in; a shell
/// that runs jobs reads none of its children; and a restore at a prompt notes nothing.
/// NEGATIVE CONTROLS (scratch): no children read (as before M2) reads `-` for `claude`; the
/// children read in a shell that runs jobs reads `claude` there; every child placed by its stamp
/// (as before the land) reads `-` for `QZP7M2A` in the block's own child; every child placed
/// as the block's own stands in as `2.1.271` for the older child; the exec evidence ignored
/// reads `pwsh` for the fork; a child skipped for the shell's file (as before the land) reads
/// `-` for the nested `pwsh`; the gone image's exec name noted as its file name too (the test
/// above's) stands in as `nvim` for `QZP7M2A` here as well; children read with no block running
/// notes them at a prompt.
#[cfg(unix)]
#[test]
fn a_restored_pwsh_command_reads_the_shells_child_that_runs_it() {
    use crate::command_ran::{ExecNames, Steering, test_id};
    use crate::holder_image::HolderImage;
    const FORK: i32 = 4243;
    const LATE: i32 = 4244;
    const NESTED: i32 = 4245;
    const GONE: i32 = 4246;
    const PWSH: &str = "/opt/pwsh/pwsh";
    /// A start five seconds after the block's C, in wall-clock microseconds; `1`: long before.
    const LATE_US: u64 = (RESTORED_C_MS + 5_000) * 1_000;
    fn in_front(_: i32) -> i32 {
        RESTORED_SHELL
    }
    fn born(pid: i32) -> u64 {
        if matches!(pid, LATE | NESTED | GONE) {
            LATE_US
        } else {
            1
        }
    }
    fn image(pgid: i32, _: Option<&HolderImage>) -> HolderImage {
        let exe = match pgid {
            RESTORED_HOLDER | LATE => Some(RESTORED_VERSIONS.into()),
            GONE => None,
            _ => Some(PWSH.into()),
        };
        let start = exe.is_some().then(|| born(pgid));
        HolderImage::of(pgid, start, exe, atpkg::image::ImageBuild::unknown(None), 0)
    }
    fn kernel(pid: i32, _: Steering<'_>) -> ExecNames {
        match pid {
            RESTORED_HOLDER | LATE => ExecNames {
                image: Some("2.1.271".into()),
                file: Some(test_id("2.1.271")),
                env_path: Some("/u/jsmith/.local/bin:/usr/bin".into()),
                ..ExecNames::default()
            },
            GONE => ExecNames::gone(Some("nvim".into())),
            _ => ExecNames {
                image: Some("pwsh".into()),
                exec: Some("pwsh".into()),
                file: Some(test_id("pwsh")),
                ..ExecNames::default()
            },
        }
    }
    fn execd(pid: i32) -> Option<u64> {
        (pid != FORK).then(|| born(pid))
    }
    fn older(_: i32) -> Vec<i32> {
        vec![RESTORED_SHELL, RESTORED_HOLDER]
    }
    fn late(_: i32) -> Vec<i32> {
        vec![RESTORED_SHELL, LATE]
    }
    fn fork(_: i32) -> Vec<i32> {
        vec![FORK]
    }
    fn nested(_: i32) -> Vec<i32> {
        vec![NESTED]
    }
    fn gone(_: i32) -> Vec<i32> {
        vec![GONE]
    }
    let restored = |typed: Option<&str>, jobs: bool, children: fn(i32) -> Vec<i32>| {
        let probes = RestoreProbes {
            read: kernel,
            in_front,
            image,
            children,
            execd,
        };
        restored_reads(typed, jobs, probes)
    };
    let read = |typed: &str, jobs: bool, children: fn(i32) -> Vec<i32>| {
        restored(Some(typed), jobs, children).0
    };
    assert_eq!(
        [
            read("claude", false, older),
            read("claude", false, late),
            read("QZP7M2A", false, late),
            read("QZP7M2A", false, older),
            read("pwsh", false, fork),
            read("pwsh", false, nested),
            read("nvim", false, gone),
            read("QZP7M2A", false, gone),
            read("claude", true, older),
        ],
        [
            thrice("claude"),
            thrice("claude"),
            thrice("2.1.271"),
            thrice("-"),
            thrice("-"),
            thrice("pwsh"),
            thrice("nvim"),
            thrice("-"),
            thrice("-"),
        ],
        "pwsh's exec'd children, placed by their birth; never in a shell that runs jobs"
    );
    assert_eq!(
        restored(None, false, late),
        (thrice("-"), 0),
        "a restore at a prompt notes no child"
    );
}

#[test]
fn survey_rows_emits_c16_facts_and_round_trips_with_drive_isolation() {
    use aterm_agent::survey_facts::{ServerHeardFact, SurveyAccountFact, SwarmSurveyFacts};

    let store = crate::session_store::new_store();
    let shell = crate::session_store::test_handle(3);
    store.write().unwrap().register(shell);

    let facts = SwarmSurveyFacts {
        node: Some("0123456789abcdef0123456789abcdef".to_owned()),
        operation_lines: Vec::new(),
        label: Some("m22".to_owned()),
        roster_unread: false,
        version: "0.146.0".to_owned(),
        window: true,
        window_last: Some(1_793_160_000),
        sessions: 1,
        ssh_host_key: Some("SHA256:abc123xyz".to_owned()),
        accounts: vec![SurveyAccountFact {
            pool: "claude".to_owned(),
            id: "c01".to_owned(),
            email: None,
            condition: "usable".to_owned(),
            enrolled: true,
            usage_5h_pct: Some(25),
            usage_7d_pct: Some(50),
            week_reset: Some(1_793_500_000),
            walled_until: None,
            last_used: Some(1_793_159_000),
            read_at: None,
            launch: None,
            token: None,
        }],
        accounts_unread: Vec::new(),
        session_accounts: std::collections::BTreeMap::new(),
        moves: None,
        server_heard: vec![ServerHeardFact {
            vault: "alab".to_owned(),
            subject: "age1ql3z7hjy54pw3hyww5ayyfg7zqgvc7w3j2elw8zmrj2kg5sfn9aqmcac8p".to_owned(),
            at: 1_793_159_999,
        }],
        home: None,
        home_dir: None,
        home_free_mb: None,
        home_total_mb: None,
        home_build: None,
        store_worst: None,
        machine: None,
        repos: None,
        update: None,
    };
    let non_drive = Here {
        host: Some("m22.local".to_owned()),
        instances: 1,
        access: aterm_types::fleet_access::FleetAccess::ReadOnly,
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

/// Home paths and checkout names need content access. Numeric free space remains available
/// at facts level without revealing either location.
#[test]
fn a_survey_says_the_home_and_its_checkouts_at_content_alone() {
    use aterm_agent::harness::repo_work::home::{HomeRepo, HomeRepos};
    use aterm_agent::survey_facts::SwarmSurveyFacts;

    let store = crate::session_store::new_store();
    let facts = SwarmSurveyFacts {
        version: "0.147.0".to_owned(),
        operation_lines: Vec::new(),
        home: Some("/Users//a".to_owned()),
        home_dir: None,
        home_free_mb: Some(812_345),
        home_total_mb: None,
        home_build: None,
        store_worst: None,
        machine: None,
        repos: Some(HomeRepos {
            read_ms: 1_793_159_000_000,
            repos: vec![HomeRepo {
                name: "trust".to_owned(),
                path: "/Users//a/trust".into(),
                head: Some("e23254f84a67131e6cad7724c7f749073b190dd2".to_owned()),
                branch: Some("main".to_owned()),
                main_ref: Some("origin/main".to_owned()),
                behind: Some(2),
                fetched_ms: Some(1_793_150_000_000),
                dirty: Some(false),
            }],
            cut: 0,
        }),
        node: None,
        label: None,
        roster_unread: false,
        window: false,
        window_last: None,
        sessions: 0,
        ssh_host_key: None,
        accounts: Vec::new(),
        accounts_unread: Vec::new(),
        session_accounts: std::collections::BTreeMap::new(),
        moves: None,
        server_heard: Vec::new(),
        update: None,
    };
    let here = Here {
        host: Some("m22.local".to_owned()),
        instances: 1,
        access: aterm_types::fleet_access::FleetAccess::ReadOnly,
        drive: true,
        facts: Some(facts.clone()),
    };
    let content = survey_rows(&store, SurveyLevel::Content, usize::MAX, &here);
    assert!(content.contains("\nrepo name=trust "), "{content}");
    let parsed = SwarmSurveyFacts::from_survey_reply(&content).expect("a content reply");
    assert_eq!(
        (&parsed.home, parsed.home_free_mb, &parsed.repos),
        (&facts.home, facts.home_free_mb, &facts.repos)
    );
    let at_facts = survey_rows(&store, SurveyLevel::Facts, usize::MAX, &here);
    assert!(
        !at_facts.contains(" home=")
            && !at_facts.contains("repo ")
            && !at_facts.contains("/Users//a"),
        "{at_facts}"
    );
    let parsed = SwarmSurveyFacts::from_survey_reply(&at_facts).expect("a facts reply");
    assert_eq!(
        (parsed.home, parsed.home_free_mb, parsed.repos),
        (None, Some(812_345), None)
    );
}

/// Where this Mac's updater stands is a resource fact: a window's survey says its `update*=` words
/// on the `instance` header at facts as at content, and a reader takes them back whole; facts that
/// read no updater say none.
/// NEGATIVE CONTROL: a header whose facts tokens drop the `update*=` words fails "the header says
/// the updater".
#[test]
fn a_survey_says_where_the_updater_stands_at_either_level() {
    use aterm_agent::survey_facts::SwarmSurveyFacts;
    use aterm_types::update_standing::{UpdateFact, UpdateStanding};

    let store = crate::session_store::new_store();
    let held = UpdateFact {
        checked_at: Some(1_793_159_400),
        to: Some("0.199.0".to_owned()),
        until: Some(1_793_170_000),
        why: Some("crash-loop"),
        tries: Some(2),
        checks: Some(false),
        ..UpdateFact::of(UpdateStanding::Held)
    };
    let facts = SwarmSurveyFacts {
        version: "0.198.0".to_owned(),
        operation_lines: Vec::new(),
        node: None,
        label: Some("m221".to_owned()),
        roster_unread: false,
        window: true,
        window_last: Some(1_793_159_000),
        sessions: 0,
        ssh_host_key: None,
        accounts: Vec::new(),
        accounts_unread: Vec::new(),
        session_accounts: std::collections::BTreeMap::new(),
        moves: None,
        server_heard: Vec::new(),
        home: None,
        home_dir: None,
        home_free_mb: None,
        home_total_mb: None,
        home_build: None,
        store_worst: None,
        machine: None,
        repos: None,
        update: Some(held.clone()),
    };
    let here = Here {
        host: Some("m221.local".to_owned()),
        instances: 1,
        access: aterm_types::fleet_access::FleetAccess::ReadOnly,
        drive: false,
        facts: Some(facts.clone()),
    };
    for level in [SurveyLevel::Facts, SurveyLevel::Content] {
        let reply = survey_rows(&store, level, usize::MAX, &here);
        let header = reply.lines().nth(1).expect("an instance header");
        assert!(
            header.contains(" version=0.198.0 ")
                && header.contains(" update=held update_at=1793159400 update_to=0.199.0 ")
                && header.contains(" update_checks=off "),
            "the header says the updater ({level:?}): {header}"
        );
        let parsed = SwarmSurveyFacts::from_survey_reply(&reply).expect("a survey reply");
        let said_at = parsed.update.as_ref().and_then(|update| update.said_at);
        assert!(
            said_at.is_some(),
            "with the clock it was said on ({level:?})"
        );
        assert_eq!(
            parsed.update,
            Some(UpdateFact {
                said_at,
                ..held.clone()
            }),
            "{level:?}"
        );
    }
    let unread = Here {
        facts: Some(SwarmSurveyFacts {
            update: None,
            ..facts
        }),
        ..here
    };
    let reply = survey_rows(&store, SurveyLevel::Facts, usize::MAX, &unread);
    assert!(!reply.contains(" update="), "{reply}");
}

/// A test binary's survey reads no checkout of the person's home: its `$HOME` is theirs, so
/// [`Here::now`] starts no git there (`aterm node serve`'s explicit home, a test's scratch one,
/// still reads).
#[test]
fn a_test_binary_reads_no_checkout_of_the_persons_home() {
    let exe = std::env::current_exe().expect("this program");
    assert!(
        aterm_uds::test_binary::is_test_binary(&exe),
        "a test binary"
    );
    assert_eq!(crate::survey::home_to_read(), None);
}

/// A record of where a worker works, for the work rows' tests: branch `feat/x` in `/repo`, three
/// behind `origin/main`, 70 files of which it names three, one holding a credential-shaped word
/// and one a fleet join code, read 40 s before `now_ms`.
fn work_record_at(now_ms: u64) -> aterm_agent::harness::work_record::WorkRecord {
    aterm_agent::harness::work_record::WorkRecord {
        host: "m7.local".into(),
        common_dir: "/repo/.git".into(),
        tree: "/repo/wt".into(),
        branch: Some("feat/x".into()),
        detached: None,
        main_ref: "origin/main".into(),
        base: Some("e23254f84a67131e6cad7724c7f749073b190dd2".into()),
        behind: Some(3),
        paths: vec![
            "src/lib.rs".into(),
            format!("keys/sk-proj-{}.env", "a1B2c3D4".repeat(4)),
            "notes/atf1-q7xk-29fa-pm3c.md".into(),
        ],
        files: 70,
        read_ms: now_ms.saturating_sub(40_000),
        starts: 11,
    }
}

/// A CONTENT ROW SAYS WHERE A WORKER WORKS, THROUGH THE REDACTOR: `work=` its branch,
/// `work_repo=` and `work_tree=`, `work_base=` its main ref at the base's first 12 hex digits,
/// `work_behind=`, `work_files=` (past the names), `work_paths=` every name it holds, one a line,
/// a credential-shaped word and a fleet join code in them redacted, and `work_ms=` the record's
/// age. NEGATIVE CONTROL: the same session's facts-level row carries no work field, so a fleet
/// peer without a content grant sees none.
#[test]
fn a_content_row_says_where_a_worker_works_through_the_redactor() {
    let store = crate::session_store::new_store();
    let claude = crate::session_store::test_handle(5);
    claude_session(&claude);
    let now = aterm_time::unix_millis().expect("the clock");
    assert!(
        claude
            .ctx
            .timeline
            .lock()
            .unwrap()
            .set_work(4242, Some(work_record_at(now)))
    );
    store.write().unwrap().register(claude);
    let row = |level| {
        let out = survey_rows(&store, level, usize::MAX, &here());
        fields(out.lines().nth(2).expect("the claude row"))
    };
    let content = row(SurveyLevel::Content);
    for (key, want) in [
        ("work", "feat/x"),
        ("work_repo", "/repo/.git"),
        ("work_tree", "/repo/wt"),
        ("work_base", "origin/main@e23254f84a67"),
        ("work_behind", "3"),
        ("work_files", "70"),
    ] {
        assert_eq!(content[key], want, "{key}: {content:?}");
    }
    let paths: Vec<&str> = content["work_paths"].split('\n').collect();
    assert_eq!(paths.len(), 3, "every name the record holds: {paths:?}");
    assert_eq!(paths[0], "src/lib.rs");
    assert!(
        !paths[1].contains("a1B2c3D4") && paths[1].contains("[redacted"),
        "a credential-shaped word is redacted: {paths:?}"
    );
    assert!(
        paths[2].starts_with("notes/atf1-[redacted]") && !paths[2].contains("q7xk"),
        "a join code is redacted: {paths:?}"
    );
    let age: u64 = content["work_ms"].parse().expect("an age");
    assert!((40_000..600_000).contains(&age), "the record's age: {age}");

    let facts = row(SurveyLevel::Facts);
    let leaked: Vec<&String> = facts.keys().filter(|k| k.starts_with("work")).collect();
    assert!(
        leaked.is_empty(),
        "CONTROL: a facts row says nothing of where it works: {leaked:?}"
    );
}

/// A RECORD SHOWS ONLY WHILE ITS GROUP IS IN FRONT: a record put for the agent's group shows in
/// its content row; one put for another group is not taken; once a shell's group is in front the
/// row shows none. NEGATIVE CONTROL: with the agent's group in front again, the same group's
/// record is shown.
#[test]
fn a_record_shows_only_while_its_group_is_in_front() {
    let store = crate::session_store::new_store();
    let claude = crate::session_store::test_handle(5);
    claude_session(&claude);
    let now = aterm_time::unix_millis().expect("the clock");
    store.write().unwrap().register(claude.clone());
    let work = || {
        let out = survey_rows(&store, SurveyLevel::Content, usize::MAX, &here());
        fields(out.lines().nth(2).expect("the row"))
            .get("work")
            .cloned()
    };
    assert_eq!(work(), None, "no record read yet");
    let mut tl = claude.ctx.timeline.lock().unwrap();
    assert!(tl.set_work(4242, Some(work_record_at(now))));
    assert!(
        !tl.set_work(4243, Some(work_record_at(now))),
        "another group's record is not taken"
    );
    drop(tl);
    assert_eq!(work().as_deref(), Some("feat/x"));
    let mut tl = claude.ctx.timeline.lock().unwrap();
    tl.note_foreground_group(4243);
    tl.set_program(4243, Some("zsh".into()));
    drop(tl);
    assert_eq!(work(), None, "a shell in front shows no record");
    claude
        .ctx
        .timeline
        .lock()
        .unwrap()
        .note_foreground_group(4242);
    assert_eq!(
        work().as_deref(),
        Some("feat/x"),
        "CONTROL: the record's own group in front again"
    );
}

/// A listener-cached snapshot whose swarm roster was unread is never served again: the next
/// survey reads the roster afresh, so a writer's moment is not repeated for a minute (one-mesh
/// E2). NEGATIVE CONTROL: the same snapshot with its roster read is served from the cache until
/// [`FACTS_CACHE_TTL`] passes, and a snapshot of another node never is.
#[test]
fn a_listener_snapshot_whose_roster_was_unread_is_read_afresh() {
    let facts = |roster_unread: bool| aterm_agent::survey_facts::SwarmSurveyFacts {
        operation_lines: Vec::new(),
        node: Some("n-1".to_owned()),
        label: (!roster_unread).then(|| "m5".to_owned()),
        roster_unread,
        version: "0.155.0".to_owned(),
        window: false,
        window_last: None,
        sessions: 0,
        ssh_host_key: None,
        moves: None,
        accounts: Vec::new(),
        accounts_unread: Vec::new(),
        session_accounts: std::collections::BTreeMap::new(),
        server_heard: Vec::new(),
        home: None,
        home_dir: None,
        home_free_mb: None,
        home_total_mb: None,
        home_build: None,
        store_worst: None,
        machine: None,
        repos: None,
        update: None,
    };
    let at = std::time::Instant::now();
    let soon = at + std::time::Duration::from_secs(1);
    let unread = Here {
        facts: Some(facts(true)),
        ..here()
    };
    let read = Here {
        facts: Some(facts(false)),
        ..here()
    };
    assert!(
        !serves_cached(at, &unread, soon, Some("n-1")),
        "an unread roster is read afresh"
    );
    assert!(
        serves_cached(at, &read, soon, Some("n-1")),
        "the control: a read roster is served from the cache"
    );
    assert!(
        !serves_cached(at, &read, at + FACTS_CACHE_TTL, Some("n-1")),
        "the control: past the TTL nothing is served"
    );
    assert!(
        !serves_cached(at, &read, soon, Some("n-2")),
        "the control: another node's snapshot is not served"
    );
}

/// A LISTENER'S CACHED FACTS TAKE THE LATEST LOOK IN AN ANSWER AND NONE IN A PUSH: facts the
/// listener kept from a minute ago go out in an answer with the machine's look as it is at
/// this reply (its own time, `m_at_ms`, within the look's keep of now) and the free space and
/// the size that look read, not with the look the snapshot held. The header that carries it
/// still ends with the access word. The same facts taken for the 3-minute push take no look (a
/// look is taken only inside an answer) and keep their free space and size.
/// NEGATIVE CONTROL (2026-11-12): with the look served from the snapshot, the reply says the
/// stale look's time and its cores.
#[test]
fn a_listeners_cached_facts_take_the_latest_look() {
    use aterm_agent::survey_facts::{MachineFacts, SwarmSurveyFacts};
    use aterm_capacity::machine_look::{LOOK_KEEP, MachineLook};

    let before = aterm_time::unix_millis().expect("a clock after the epoch");
    // What the snapshot holds: a look of another moment, with figures no machine here reads.
    let stale = MachineFacts {
        look: MachineLook {
            at_unix_ms: Some(1),
            cores: Some(4096),
            disk_free_mb: Some(7),
            ..MachineLook::UNREAD
        },
        up_since_ms: None,
        os: None,
        target: None,
    };
    let mut facts = SwarmSurveyFacts {
        operation_lines: Vec::new(),
        node: Some("n-1".to_owned()),
        label: Some("m5".to_owned()),
        roster_unread: false,
        version: "0.199.0".to_owned(),
        window: true,
        window_last: None,
        sessions: 0,
        ssh_host_key: None,
        moves: None,
        accounts: Vec::new(),
        accounts_unread: Vec::new(),
        session_accounts: std::collections::BTreeMap::new(),
        server_heard: Vec::new(),
        home: None,
        home_dir: None,
        home_free_mb: Some(7),
        home_total_mb: Some(9),
        home_build: None,
        store_worst: None,
        machine: None,
        repos: None,
        update: None,
    };
    let mut pushed = facts.clone();
    take_latest(&mut pushed, Taking::NoLook);
    assert_eq!(
        (pushed.machine, pushed.home_free_mb, pushed.home_total_mb),
        (None, Some(7), Some(9)),
        "the push takes no look"
    );
    facts.machine = Some(stale);
    take_latest(&mut facts, Taking::Look);
    let machine = facts.machine.clone().expect("the facts still carry a look");
    let at = machine.look.at_unix_ms.expect("a look taken on this clock");
    let keep = u64::try_from(LOOK_KEEP.as_millis()).expect("ten seconds");
    assert!(
        at >= before.saturating_sub(keep),
        "a look of this reply, not the snapshot's: {at} before {before}"
    );
    assert_eq!(
        machine.look.cores,
        u32::try_from(aterm_sysprobe::cpu_layout().logical_cores()).ok(),
        "this machine's cores"
    );
    assert_eq!(
        (
            facts.home_free_mb,
            facts.home_total_mb,
            machine.look.disk_free_mb
        ),
        (None, None, None),
        "the free space and the size are the look's: no home, no volume read"
    );

    let store = crate::session_store::new_store();
    let here = Here {
        facts: Some(facts),
        ..here()
    };
    let out = survey_rows(&store, SurveyLevel::Facts, usize::MAX, &here);
    let head = out.lines().nth(1).expect("the instance line");
    assert_eq!(fields(head)["m_at_ms"], at.to_string(), "{head}");
    assert!(
        head.contains(
            " home_free_mb=- home_total_mb=- home_build_mb=- home_build_at_ms=- \
             home_build_partial=- os="
        ),
        "{head}"
    );
    assert!(
        head.ends_with(&format!(
            " roster_unread=0 store_worst=- store_worst_pct=- access={}",
            here.access.word()
        )),
        "{head}"
    );
}

/// Holds until the wall clock is past `at_ms`, so what moves next moves later than `at_ms`.
fn past(at_ms: u64) {
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the clock to move", || {
        (aterm_gui_timing::wall_ms() > at_ms).then_some(())
    });
}

/// `h`'s agent (`claude_session`'s) says `words`, its row written at `at_ms` where it says, and
/// is doing `doing`.
fn said(h: &SessionHandle, words: &str, at_ms: Option<u64>, doing: Option<&str>) {
    h.ctx.timeline.lock().unwrap().set_claude_excerpts(
        4242,
        Some(ClaudeExcerpts {
            excerpts: Excerpts {
                prompts: Vec::new(),
                said: Some(words.into()),
                said_end: Some(words.into()),
                said_at_ms: at_ms.map(|at| i64::try_from(at).unwrap()),
                doing: doing.map(str::to_owned),
            },
            task: None,
            transcript_len: None,
        }),
    );
}

/// `h` posts `text` on a topic channel; when it was queued.
fn topic_post(h: &SessionHandle, text: &str) -> u64 {
    let answer =
        crate::fabric::cmd_post(&*h.ctx, &format!("to=say:build kind=note -- {text}"), None);
    assert!(answer.starts_with("OK "), "{answer}");
    h.ctx.fabric.last_topic_post().expect("a topic post").at_ms
}

#[test]
fn a_step_is_the_last_topic_post_where_it_came_after_the_screens_words_and_not_before() {
    let h = crate::session_store::test_handle(5);
    claude_session(&h);
    assert_eq!(
        last_words(&h, false).as_deref(),
        Some("Running the tests now.")
    );
    let read = h.ctx.timeline.lock().unwrap().words_read();
    past(read.at(Word::Said));
    topic_post(&h, "crate 3 of 9");
    assert_eq!(
        last_words(&h, false).as_deref(),
        Some("crate 3 of 9"),
        "a newer topic post is the step"
    );
    // The same words read again (a new prompt beside them is no word) are no move: the post stays
    // the step.
    said(&h, "Running the tests now.", None, Some("Bash"));
    assert_eq!(last_words(&h, false).as_deref(), Some("crate 3 of 9"));

    let posted = h.ctx.fabric.last_topic_post().expect("a topic post").at_ms;
    past(posted);
    said(&h, "All green; landing.", None, Some("Bash"));
    assert_eq!(
        last_words(&h, false).as_deref(),
        Some("All green; landing."),
        "an older topic post is not"
    );
}

/// THE TOOL CALL THAT POSTED ENDS, AND THE POST STAYS THE STEP: `doing` moving is no move of the
/// `said` a step shows. CONTROL: `said` moving after the post is.
#[test]
fn a_step_keeps_a_topic_post_when_only_the_tool_in_flight_moves_after_it() {
    let h = crate::session_store::test_handle(5);
    claude_session(&h);
    said(&h, "Building the workspace.", None, Some("Bash"));
    past(h.ctx.timeline.lock().unwrap().words_read().at(Word::Said));
    let posted = topic_post(&h, "crate 9 of 9");
    past(posted);
    said(&h, "Building the workspace.", None, None);
    assert!(
        h.ctx.timeline.lock().unwrap().words_read().at(Word::Doing) > posted,
        "PRECONDITION: the tool's end was read after the post"
    );
    assert_eq!(
        last_words(&h, false).as_deref(),
        Some("crate 9 of 9"),
        "the call's end leaves the post the step"
    );
    said(&h, "Built; testing.", None, None);
    assert_eq!(last_words(&h, false).as_deref(), Some("Built; testing."));
}

/// WORDS WRITTEN BEFORE A POST BUT READ AFTER IT ARE OLDER THAN IT: the step goes by the time
/// the transcript row says (`said_at_ms`), not the read. CONTROL: words its row says were written
/// after the post are the step.
#[test]
fn a_step_goes_by_when_the_transcript_wrote_the_words_not_when_aterm_read_them() {
    let h = crate::session_store::test_handle(5);
    claude_session(&h);
    let posted = topic_post(&h, "crate 3 of 9");
    past(posted);
    said(
        &h,
        "Building the workspace.",
        Some(posted - 1),
        Some("Bash"),
    );
    assert!(
        h.ctx.timeline.lock().unwrap().words_read().at(Word::Said) > posted,
        "PRECONDITION: the words were read after the post"
    );
    assert_eq!(last_words(&h, false).as_deref(), Some("crate 3 of 9"));
    said(
        &h,
        "Building the workspace.",
        Some(posted + 1),
        Some("Bash"),
    );
    assert_eq!(
        last_words(&h, false).as_deref(),
        Some("Building the workspace."),
        "words written after the post"
    );
}

#[test]
fn a_step_of_words_and_a_post_that_tie_is_the_screens() {
    let post = |at_ms| crate::fabric::TopicPost {
        topic: "build".into(),
        text: "crate 3\nof 9".into(),
        at_ms,
    };
    let words = || Some(("Running".to_owned(), 5));
    assert_eq!(step_of(words(), Some(&post(5))).as_deref(), Some("Running"));
    assert_eq!(step_of(words(), Some(&post(4))).as_deref(), Some("Running"));
    assert_eq!(
        step_of(words(), Some(&post(6))).as_deref(),
        Some("crate 3 of 9")
    );
    assert_eq!(
        step_of(None, Some(&post(0))).as_deref(),
        Some("crate 3 of 9")
    );
    assert_eq!(step_of(words(), None).as_deref(), Some("Running"));
    assert_eq!(step_of(None, None), None);
}

/// `holder=` adds one counted row, this host's kernel on exactly that identity, at either level.
/// NEGATIVE CONTROLS: a request without it has no such row, and a malformed or repeated word is
/// refused rather than read as some other identity.
#[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
#[test]
fn a_holder_query_adds_a_counted_kernel_judgement_of_exactly_that_identity() {
    use aterm_ctl::release_holder::Identity;
    let store = crate::session_store::new_store();
    let pid = std::process::id();
    let (boot, start) = aterm_agent::release_progress::identity(pid).unwrap();
    let me = Identity { pid, boot, start };
    let reused = Identity {
        start: start + 1,
        ..me.clone()
    };
    let judged = |rest: &str, asked: &Identity| {
        let reply = cmd_survey(&store, rest, SurveyLevel::Content, usize::MAX, &here());
        let mut lines = reply.lines();
        let count: usize = lines.next().unwrap()["OK ".len()..].parse().unwrap();
        let rows: Vec<&str> = lines.collect();
        assert_eq!(rows.len(), count, "{reply}");
        rows.iter()
            .filter_map(|row| asked.read(row))
            .collect::<Vec<_>>()
    };
    assert_eq!(judged(&me.arg(), &me), [Some(true)]);
    assert_eq!(judged(&format!("facts {}", me.arg()), &me), [Some(true)]);
    assert_eq!(
        judged(&format!("content {}", reused.arg()), &reused),
        [Some(false)]
    );
    assert!(judged("facts", &me).is_empty());
    // The word that names the session that asked changes no answer, wherever it stands: a tab
    // ID, or the ID a legacy tab's shell still holds.
    let by = "by=t0123456789";
    let legacy = "by=s-0123456789abcdef0123";
    // The header's clock (`at_ms=`) moves between two asks; nothing else may.
    let plain = |rest: &str| {
        cmd_survey(&store, rest, SurveyLevel::Content, usize::MAX, &here())
            .lines()
            .map(|line| {
                line.split(' ')
                    .map(|word| {
                        if word.starts_with("at_ms=") {
                            "at_ms=-"
                        } else {
                            word
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(plain(&format!("facts {by}")), plain("facts"));
    assert_eq!(plain(by), plain(""));
    assert_eq!(plain(legacy), plain(""));
    assert_eq!(
        judged(&format!("{by} facts {}", me.arg()), &me),
        [Some(true)]
    );
    for bad in [
        "holder=1.ab".to_owned(),
        "facts content".to_owned(),
        format!("{} {}", me.arg(), me.arg()),
        format!("{} everything", me.arg()),
        format!("{by} {by}"),
        format!("{by} {legacy}"),
        "by=someone".to_owned(),
        "by=".to_owned(),
        // The loose `s-` forms name no session: any hex, upper case, another length.
        "by=s-5e55".to_owned(),
        "by=s-0123456789ABCDEF0123".to_owned(),
        "by=s-0123456789abcdef012".to_owned(),
    ] {
        assert!(
            cmd_survey(&store, &bad, SurveyLevel::Content, usize::MAX, &here())
                .starts_with("ERR usage: survey"),
            "{bad}"
        );
    }
}

#[test]
fn a_request_tab_row_names_its_request_from_and_why() {
    let _here = RequestsHere::new();
    let store = crate::session_store::new_store();
    let tab = crate::session_store::test_handle(41);
    let person = crate::session_store::test_handle(42);
    let id = crate::requests_host::opened_for_test(
        true,
        "87be5228c0ffee0087be5228c0ffee00",
        "m114",
        "fleet:87be5228:tqn5r6e0148",
        crate::requests_host::Made::Shell,
        tab.sid.as_str(),
    );
    let (sid, person_sid) = (tab.sid.as_str().to_owned(), person.sid.as_str().to_owned());
    store.write().unwrap().register(tab);
    store.write().unwrap().register(person);
    let row_of = |level, sid: &str| {
        survey_rows(&store, level, usize::MAX, &here())
            .lines()
            .find(|line| line.contains(&format!(" sid={sid} ")))
            .map(fields)
            .unwrap_or_else(|| panic!("a row for {sid}"))
    };
    for level in [SurveyLevel::Facts, SurveyLevel::Content] {
        let row = row_of(level, &sid);
        assert_eq!(row["request"], id, "{level:?}");
        assert_eq!(row["request_from"], "m114");
        assert_eq!(
            row["request_why"], "a test's request",
            "a given why is host tier"
        );
        // NEGATIVE: a person's tab names no request.
        let mine = row_of(level, &person_sid);
        assert_eq!(
            (
                mine["request"].as_str(),
                mine["request_from"].as_str(),
                mine["request_why"].as_str()
            ),
            ("-", "-", "-")
        );
    }
}

/// This test thread's requests in a folder of its own until dropped.
struct RequestsHere {
    _dir: aterm_tempfile::TempDir,
}

impl RequestsHere {
    fn new() -> Self {
        let dir = aterm_tempfile::tempdir().expect("a folder");
        crate::requests_host::set_dir_for_test(Some(dir.path().to_path_buf()));
        Self { _dir: dir }
    }
}

impl Drop for RequestsHere {
    fn drop(&mut self) {
        assert!(
            crate::requests_host::flush_within(std::time::Duration::from_secs(60)),
            "the writer flushed"
        );
        crate::requests_host::forget_for_test();
        crate::requests_host::set_dir_for_test(None);
    }
}

#[test]
fn a_fallback_why_reads_one_to_a_drive_as_at_facts() {
    let _here = RequestsHere::new();
    let store = crate::session_store::new_store();
    let (worker, shell) = (
        crate::session_store::test_handle(43),
        crate::session_store::test_handle(44),
    );
    let asker = (
        true,
        "87be5228c0ffee0087be5228c0ffee00",
        "m114",
        "fleet:87be5228:tqn5r6e0148",
    );
    let briefed = crate::requests_host::opened_why_for_test(
        asker,
        crate::requests_host::Made::Agent("claude"),
        worker.sid.as_str(),
        Err("fix the parser's private bug"),
    );
    let given = crate::requests_host::opened_for_test(
        asker.0,
        asker.1,
        asker.2,
        asker.3,
        crate::requests_host::Made::Shell,
        shell.sid.as_str(),
    );
    let (worker_sid, shell_sid) = (
        worker.sid.as_str().to_owned(),
        shell.sid.as_str().to_owned(),
    );
    store.write().unwrap().register(worker);
    store.write().unwrap().register(shell);
    let row_of = |level, sid: &str| {
        survey_rows(&store, level, usize::MAX, &here())
            .lines()
            .find(|line| line.contains(&format!(" sid={sid} ")))
            .map(fields)
            .unwrap_or_else(|| panic!("a row for {sid}"))
    };
    // This Mac's own connection reads the fallback at content, `1` at facts.
    let row = row_of(SurveyLevel::Content, &worker_sid);
    assert_eq!(row["request"], briefed);
    assert_eq!(row["request_why"], "fix the parser's private bug");
    assert_eq!(row_of(SurveyLevel::Facts, &worker_sid)["request_why"], "1");
    // Another Mac's drive reads no content tier: the fallback is `1` at content too.
    let _driving = crate::fabric::DrivingAs::enter(crate::fabric::Driver {
        node: "c0ffee00c0ffee00c0ffee00c0ffee00".to_owned(),
        host: "m72".to_owned(),
    });
    let row = row_of(SurveyLevel::Content, &worker_sid);
    assert_eq!(
        (row["request"].as_str(), row["request_why"].as_str()),
        (briefed.as_str(), "1")
    );
    // NEGATIVE: a requester's own why is the host tier, which a drive reads.
    let row = row_of(SurveyLevel::Content, &shell_sid);
    assert_eq!(
        (row["request"].as_str(), row["request_why"].as_str()),
        (given.as_str(), "a test's request")
    );
}

/// `h` with `program` in front in group `pgid`, its reader `reader`'s, at rest.
fn agent_in_front(h: &SessionHandle, pgid: i32, program: &str, reader: aterm_phase::Program) {
    let mut tl = h.ctx.timeline.lock().unwrap();
    tl.note_foreground_group(pgid);
    tl.set_program(pgid, Some(program.into()));
    tl.publish_agent(
        "idle",
        None,
        None,
        Some(reader),
        crate::session_timeline::AgentStamp {
            generation: crate::control::ScreenGen { epoch: 1, seq: 7 },
            fp: 0x5eed,
        },
    );
}

/// The status sweep over `h` with its shell's group 100 and `pgid` in front: the session log
/// opens the run of the program there.
fn swept(h: &SessionHandle, pgid: i32) {
    let mut tl = h.ctx.timeline.lock().unwrap();
    tl.note_root_group(100);
    let _ = tl.sweep_slog(&crate::session_log::BlockRead::default(), false, 100, pgid);
}

/// `h`'s agent in front (group `pgid`, its program's run opened by the status sweep) with its
/// transcript join bound: `source`'s tap bound a file, as a live Claude Code's (or a
/// codex-alab's rollout) does once its tap finds it. Only then does a row count its tool steps.
fn join_bound(h: &SessionHandle, pgid: i32, source: crate::session_log::transcript::Source) {
    use aterm_agent::harness::steps::{Step, StepBatch, StepEvent, StepPos, StepState};
    swept(h, pgid);
    let bind = StepBatch {
        steps: vec![Step {
            pos: StepPos {
                fp: 1,
                id: None,
                off: 0,
                at_ms: 0,
            },
            state: StepState::Partial,
            ev: StepEvent::Bind { len: 0 },
        }],
        ..StepBatch::default()
    };
    let _ = crate::session_log::transcript::apply_batch(&h.ctx.timeline, source, &bind, false);
}

/// `h`'s facts row as read at `now` (Unix ms).
fn row_at(h: &SessionHandle, now: u64) -> std::collections::HashMap<String, String> {
    fields(
        &session_row(
            h,
            SurveyLevel::Facts,
            None,
            (crate::metrics::now_us(), now, 0),
        )
        .text,
    )
}

/// `f` over `h`'s session log.
fn in_log<R>(h: &SessionHandle, f: impl FnOnce(&mut aterm_slog::Log) -> R) -> R {
    let mut tl = h.ctx.timeline.lock().unwrap();
    f(tl.slog_mut().log_mut_for_tests())
}

/// The stamp of wall-clock instant `at` (Unix ms).
const fn wall(at: u64) -> aterm_slog::Stamp {
    aterm_slog::Stamp::new(at, at)
}

/// An agent run opened at `run_at` and a turn under it opened at `turn_at`: the turn.
fn open_turn(log: &mut aterm_slog::Log, run_at: u64, turn_at: u64) -> aterm_slog::Node {
    use aterm_slog::{Conf, Kind, Lvl, Open, Src};
    let run = match log.deepest_open(Lvl::Run) {
        Some(run) => run,
        None => log
            .open(
                Open::new(Kind::Agent, Src::Status, Conf::Strong),
                wall(run_at),
            )
            .expect("the agent run"),
    };
    log.open(
        Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
        wall(turn_at),
    )
    .expect("the turn")
}

/// A tool step the transcript opens under `turn` at `at`.
fn open_tool(log: &mut aterm_slog::Log, turn: aterm_slog::Node, at: u64) -> aterm_slog::Node {
    use aterm_slog::{Conf, Fields, Key, Kind, Open, Src};
    log.open(
        Open::new(Kind::Tool, Src::Transcript, Conf::Strong)
            .under(turn)
            .by(aterm_slog::Actor::Vendor)
            .fields(Fields::new().word(Key::Cls, "shell").expect("cls")),
        wall(at),
    )
    .expect("the tool step")
}

fn close_turn(log: &mut aterm_slog::Log, turn: aterm_slog::Node, at: u64) {
    use aterm_slog::{Close, Conf, Out, Src, TurnOut};
    log.close(
        turn,
        Close::new(Out::Turn(TurnOut::Done), Src::Screen, Conf::Strong),
        wall(at),
    )
    .expect("the turn closes");
}

fn close_tool(log: &mut aterm_slog::Log, step: aterm_slog::Node, at: u64) {
    use aterm_slog::{Close, Conf, Out, Src, StepOut};
    log.close(
        step,
        Close::new(Out::Step(StepOut::Ok), Src::Transcript, Conf::Strong),
        wall(at),
    )
    .expect("the step closes");
}

/// The eleven keys a row says its agent's turn, step, output and tokens with.
const PROGRESS_KEYS: [&str; 11] = [
    "turn",
    "turn_ms",
    "turn_end_ms",
    "step_ms",
    "out_ms",
    "out_src",
    "tok_in",
    "tok_out",
    "tok_for_ms",
    "turns",
    "tools",
];

/// A SESSION SAYS ITS OPEN TURN AND HOW LONG IT HAS BEEN OPEN: `turn=1` and `turn_ms=` from its
/// session log's open turn (when it opened), never the reader's word age (`agent_ms=`, which
/// moves with every screen word); once the turn closes, `turn=0`, no open age, and
/// `turn_end_ms=` how long ago it closed. `turns=` counts the turns opened.
#[test]
fn a_session_says_its_open_turn_and_how_long_it_has_been_open() {
    let h = crate::session_store::test_handle(21);
    agent_in_front(&h, 4242, "claude", aterm_phase::Program::Claude);
    let now = aterm_gui_timing::wall_ms();
    let before = row_at(&h, now);
    assert_eq!(before["turn"], "0", "no turn yet: {before:?}");
    let turns: u32 = before["turns"].parse().expect("a count");
    let turn = in_log(&h, |log| open_turn(log, now - 600_000, now - 90_000));
    let open = row_at(&h, now);
    assert_eq!(
        (open["turn"].as_str(), open["turn_ms"].as_str()),
        ("1", "90000"),
        "{open:?}"
    );
    assert_ne!(open["agent_ms"], "90000", "PRECONDITION: two ages apart");
    assert_eq!(open["turns"], (turns + 1).to_string(), "{open:?}");
    in_log(&h, |log| close_turn(log, turn, now - 30_000));
    let closed = row_at(&h, now);
    assert_eq!(
        (
            closed["turn"].as_str(),
            closed["turn_ms"].as_str(),
            closed["turn_end_ms"].as_str()
        ),
        ("0", "-", "30000"),
        "{closed:?}"
    );
}

/// A SESSION SAYS ITS OPEN STEP AND WHEN ITS LAST TURN CLOSED: `step_ms=` is the age of the step
/// open now (a tool call in flight), never the age of the newest one that closed; with none open
/// it is `-`. `turn_end_ms=` is the previous turn's close while a newer one is open, and a
/// transcript reader's last output is its newest step (`out_src=transcript`). `tools=` counts
/// tool steps.
#[test]
fn a_session_says_its_open_step_and_when_its_last_turn_closed() {
    let h = crate::session_store::test_handle(22);
    agent_in_front(&h, 4242, "claude", aterm_phase::Program::Claude);
    join_bound(&h, 4242, crate::session_log::transcript::Source::Transcript);
    let now = aterm_gui_timing::wall_ms();
    let tools: u32 = row_at(&h, now)["tools"].parse().expect("a count");
    let step = in_log(&h, |log| {
        let first = open_turn(log, now - 900_000, now - 800_000);
        close_turn(log, first, now - 400_000);
        let turn = open_turn(log, now - 900_000, now - 300_000);
        let done = open_tool(log, turn, now - 200_000);
        close_tool(log, done, now - 150_000);
        open_tool(log, turn, now - 40_000)
    });
    let row = row_at(&h, now);
    for (key, want) in [
        ("turn", "1"),
        ("turn_ms", "300000"),
        ("turn_end_ms", "400000"),
        ("step_ms", "40000"),
        ("out_ms", "40000"),
        ("out_src", "transcript"),
    ] {
        assert_eq!(row[key], want, "{key}: {row:?}");
    }
    assert_eq!(row["tools"], (tools + 2).to_string(), "{row:?}");
    in_log(&h, |log| close_tool(log, step, now - 10_000));
    let row = row_at(&h, now);
    assert_eq!(
        (row["step_ms"].as_str(), row["out_ms"].as_str()),
        ("-", "10000"),
        "no step open; the closed one is the newest output: {row:?}"
    );
}

/// A SPINNER MOVES THE SCREEN AND NOT THE TRANSCRIPT'S LAST OUTPUT: a Claude Code row's
/// `out_ms=` is its transcript's newest time (the agent's newest text row, the API's last
/// served row), however recently its screen moved: a spinner keeps the screen fresh while the
/// agent writes nothing.
#[test]
fn a_spinner_moves_the_screen_and_not_the_transcripts_last_output() {
    let h = crate::session_store::test_handle(23);
    agent_in_front(&h, 4242, "claude", aterm_phase::Program::Claude);
    assert!(
        h.ctx
            .screen_moves
            .fed(crate::control::ScreenGen { epoch: 1, seq: 99 }),
        "a spinner's frame moves the screen"
    );
    let now = aterm_gui_timing::wall_ms();
    let moved = h.ctx.screen_moves.moved_ms();
    assert!(moved > 0 && now - moved < 300_000, "PRECONDITION: {moved}");
    let said = |tl: &mut crate::session_timeline::SessionTimeline, at: u64| {
        tl.set_claude_excerpts(
            4242,
            Some(ClaudeExcerpts {
                excerpts: Excerpts {
                    said: Some("Building the release.".into()),
                    said_at_ms: Some(i64::try_from(at).unwrap()),
                    ..Excerpts::default()
                },
                ..ClaudeExcerpts::default()
            }),
        );
    };
    said(&mut h.ctx.timeline.lock().unwrap(), now - 300_000);
    let row = row_at(&h, now);
    assert_eq!(
        (row["out_ms"].as_str(), row["out_src"].as_str()),
        ("300000", "transcript"),
        "{row:?}"
    );
    let served_s = (now - 120_000) / 1000;
    assert!(h.ctx.timeline.lock().unwrap().set_claude_footer(
        4242,
        None,
        Some(FooterFacts {
            usage: Some(aterm_agent::harness::session_usage::UsageFacts {
                served_at: Some(i64::try_from(served_s).unwrap()),
                ..Default::default()
            }),
            ..FooterFacts::default()
        }),
    ));
    let row = row_at(&h, now);
    assert_eq!(
        (row["out_ms"].clone(), row["out_src"].as_str()),
        ((now - served_s * 1000).to_string(), "transcript"),
        "the API served a row after the newest text: {row:?}"
    );
}

/// A whole conversation's usage as Claude Code's footer reads it: 120,000 tokens in and 8,000
/// out, `complete` the refresh read every file, `unread` subagent files it did not, `walk_cut` a
/// walk cut short.
fn claude_usage(
    complete: bool,
    unread: usize,
    walk_cut: bool,
) -> aterm_agent::harness::session_usage::UsageFacts {
    aterm_agent::harness::session_usage::UsageFacts {
        models: vec![aterm_agent::harness::session_usage::ModelTokens {
            label: "opus".into(),
            input: 120_000,
            output: 8_000,
        }],
        complete,
        unread,
        unread_more: walk_cut,
        ..Default::default()
    }
}

/// Claude Code's footer for `conversation` with `usage`.
fn usage_footer(
    usage: aterm_agent::harness::session_usage::UsageFacts,
    conversation: &str,
) -> FooterFacts {
    FooterFacts {
        usage: Some(usage),
        owner: Some(aterm_agent::harness::footer::FactsOwner {
            floor: None,
            session_id: conversation.into(),
        }),
        ..FooterFacts::default()
    }
}

/// TOKEN COUNTERS ARE DASHES UNTIL THE USAGE READ IS WHOLE: a Claude Code row sends
/// `tok_in= tok_out=` only once the refresh that folded them read every file of the
/// conversation, its subagents' included and its walk not cut short; before, the totals are a
/// prefix, and sent as counts a restart's catch-up would read as one minute's burst. A codex-alab
/// row sends its rollout's own running total, and `-` for a counter it does not give.
#[test]
fn token_counters_are_dashes_until_the_usage_read_is_whole() {
    let h = crate::session_store::test_handle(24);
    agent_in_front(&h, 4242, "claude", aterm_phase::Program::Claude);
    let now = aterm_gui_timing::wall_ms();
    let tokens = |h: &SessionHandle, at: u64| {
        let row = row_at(h, at);
        [
            row["tok_in"].clone(),
            row["tok_out"].clone(),
            row["tok_for_ms"].clone(),
        ]
    };
    let dashes = ["-", "-", "-"].map(str::to_owned);
    assert_eq!(tokens(&h, now), dashes, "no usage read");
    for (usage, why) in [
        (
            claude_usage(false, 0, false),
            "the refresh has not caught up",
        ),
        (claude_usage(true, 2, false), "two subagent files unread"),
        (claude_usage(true, 0, true), "the walk was cut short"),
    ] {
        assert!(h.ctx.timeline.lock().unwrap().set_claude_footer(
            4242,
            None,
            Some(usage_footer(usage, "conv-a"))
        ));
        assert_eq!(tokens(&h, now), dashes, "{why}");
    }
    assert!(h.ctx.timeline.lock().unwrap().set_claude_footer(
        4242,
        None,
        Some(usage_footer(claude_usage(true, 0, false), "conv-a"))
    ));
    assert_eq!(tokens(&h, now), ["120000", "8000", "0"].map(str::to_owned));
    assert_eq!(
        tokens(&h, now + 60_000),
        ["120000", "8000", "60000"].map(str::to_owned),
        "whole since the first row that sent them"
    );

    let codex = crate::session_store::test_handle(25);
    codex_session(&codex);
    assert!(
        codex
            .ctx
            .timeline
            .lock()
            .unwrap()
            .set_codex_survey(CODEX_PGID, codex_survey())
    );
    assert_eq!(tokens(&codex, now), dashes, "a rollout that named no total");
    let survey = codex_survey();
    assert!(codex.ctx.timeline.lock().unwrap().set_codex_survey(
        CODEX_PGID,
        CodexSurvey {
            facts: aterm_agent::harness::codex_facts::CodexFacts {
                tok_in: Some(95_000),
                tok_out: None,
                ..survey.facts.clone()
            },
            ..survey
        }
    ));
    assert_eq!(tokens(&codex, now), ["95000", "-", "0"].map(str::to_owned));
}

/// THE TOKEN EPOCH RESTARTS WITH THE CONVERSATION: `tok_for_ms=` is how long the counters have
/// counted the conversation in front unbroken, so an observer takes a rate only inside it. A new
/// or resumed conversation, the same conversation in a restarted process, and a read that is not
/// whole each start it again.
#[test]
fn the_token_epoch_restarts_with_the_conversation() {
    let h = crate::session_store::test_handle(26);
    agent_in_front(&h, 4242, "claude", aterm_phase::Program::Claude);
    let t0 = aterm_gui_timing::wall_ms();
    let footer = |pgid: i32, conversation: &str, whole: bool| {
        assert!(h.ctx.timeline.lock().unwrap().set_claude_footer(
            pgid,
            None,
            Some(usage_footer(claude_usage(whole, 0, false), conversation))
        ));
    };
    let for_ms = |at: u64| row_at(&h, t0 + at)["tok_for_ms"].clone();
    footer(4242, "conv-a", true);
    assert_eq!(for_ms(0), "0");
    assert_eq!(for_ms(60_000), "60000");
    footer(4242, "conv-b", true);
    assert_eq!(for_ms(120_000), "0", "a resumed conversation counts anew");
    assert_eq!(for_ms(180_000), "60000");
    agent_in_front(&h, 4243, "claude", aterm_phase::Program::Claude);
    footer(4243, "conv-b", true);
    assert_eq!(
        for_ms(240_000),
        "0",
        "the same conversation, a restarted process"
    );
    footer(4243, "conv-b", false);
    assert_eq!(for_ms(300_000), "-", "a read that is not whole");
    footer(4243, "conv-b", true);
    assert_eq!(for_ms(360_000), "0", "whole again: counting anew");
    // A conversation that comes while the clock is unread (`0`) starts no epoch: its counters
    // go out and their age does not, and its epoch starts at the first read clock, never at 0
    // (which every later row would send as the whole Unix time).
    footer(4243, "conv-c", true);
    let unread = row_at(&h, 0);
    assert_eq!(
        (unread["tok_in"].as_str(), unread["tok_for_ms"].as_str()),
        ("120000", "-"),
        "{unread:?}"
    );
    assert_eq!(
        for_ms(420_000),
        "0",
        "the epoch starts at the first read clock"
    );
    assert_eq!(for_ms(480_000), "60000");
}

/// A TOOL COUNT IS UNREAD UNTIL THE TRANSCRIPT JOIN BINDS: a Claude Code or codex-alab row's
/// tool steps reach the session log only through its transcript (rollout) join, so while the
/// join binds nothing (no file found yet, a rollout tap that refused) the row says `tools=-`,
/// never a count of 0 tool calls, however many it ran; its turns still count. Once the join is
/// bound the row counts every tool step its run holds.
/// CONTROL: the bound check removed (a reader's count sent whatever its join holds) is caught.
#[test]
fn a_tool_count_is_unread_until_the_transcript_join_binds() {
    use crate::session_log::transcript::Source;
    for (id, pgid, program, reader, source) in [
        (
            30,
            7101,
            "claude",
            aterm_phase::Program::Claude,
            Source::Transcript,
        ),
        (
            31,
            7102,
            "codex",
            aterm_phase::Program::Codex,
            Source::Rollout,
        ),
    ] {
        let h = crate::session_store::test_handle(id);
        agent_in_front(&h, pgid, program, reader);
        swept(&h, pgid);
        let now = aterm_gui_timing::wall_ms();
        in_log(&h, |log| {
            let turn = open_turn(log, now - 900_000, now - 600_000);
            let step = open_tool(log, turn, now - 500_000);
            close_tool(log, step, now - 400_000);
        });
        let row = row_at(&h, now);
        assert_eq!(row["tools"], "-", "{program}: no join bound: {row:?}");
        assert_ne!(row["turns"], "-", "{program}: {row:?}");
        join_bound(&h, pgid, source);
        assert!(
            h.ctx
                .timeline
                .lock()
                .unwrap()
                .slog()
                .transcript_whole(reader),
            "PRECONDITION: {program}'s join is bound"
        );
        let row = row_at(&h, now);
        let tools: u32 = row["tools"]
            .parse()
            .unwrap_or_else(|_| panic!("{program}: a count once bound: {row:?}"));
        assert!(tools >= 1, "{program}: {row:?}");
    }
}

/// `batch` applied by `source`'s tap to `h`'s session log, as its tap hands it on.
fn tap_batch(
    h: &SessionHandle,
    source: crate::session_log::transcript::Source,
    batch: &aterm_agent::harness::steps::StepBatch,
) {
    let _ = crate::session_log::transcript::apply_batch(&h.ctx.timeline, source, batch, true);
}

/// A TOOL COUNT IS UNREAD ONCE THE JOIN STOPS HOLDING IT WHOLE: a bound join puts its run's tool
/// steps in the log only while it reads them all. After its fold latches `state=off` on a row it
/// could not read (every later step of that file dropped), or after it lost rows (the fold's
/// `Lost` rows, a queue's overflow), the log's count is frozen or short, so the row says `tools=-`
/// rather than that count as exact, for the rest of the run: a bind of another file reads that
/// file's rows, not the ones lost. Turns still count. A reader that stopped (a rollout tap
/// dropped; Claude Code's footer resolver stopped, its tab status turned off) reads nothing, so
/// its row says `-` until a transcript fact says it reads again, and then counts.
/// CONTROLS: the latch's row not counted lost (`TxEvent::Off` adding nothing, the latch case, at
/// once or after the rebind), the `dropped=` check removed (the loss and overflow cases), the
/// stopped check removed (the stopped cases) and the Claude stop leaving its run unmarked (the
/// Claude stopped case) are each caught.
#[test]
fn a_tool_count_is_unread_once_the_join_stops_holding_it_whole() {
    use crate::session_log::transcript::Source;
    use aterm_agent::harness::steps::{Step, StepBatch, StepEvent, StepPos, StepState};
    let at = |fp: u64, ev: StepEvent| StepBatch {
        steps: vec![Step {
            pos: StepPos {
                fp,
                id: None,
                off: 0,
                at_ms: 0,
            },
            state: StepState::Partial,
            ev,
        }],
        ..StepBatch::default()
    };
    let off = at(1, StepEvent::Off { why: "unknown" });
    let another_file = at(2, StepEvent::Bind { len: 0 });
    // A lost row of the bound file, past its watermark.
    let lost = StepBatch {
        steps: vec![Step {
            pos: StepPos {
                fp: 1,
                id: None,
                off: 1,
                at_ms: 0,
            },
            state: StepState::Partial,
            ev: StepEvent::Lost { n: 1 },
        }],
        ..StepBatch::default()
    };
    let overflow = StepBatch {
        overflow: 1,
        ..StepBatch::default()
    };
    let mut id = 40;
    for (program, reader, source) in [
        ("claude", aterm_phase::Program::Claude, Source::Transcript),
        ("codex", aterm_phase::Program::Codex, Source::Rollout),
    ] {
        for (case, batch) in [("off", &off), ("lost", &lost), ("overflow", &overflow)] {
            id += 1;
            let pgid = 7200 + i32::try_from(id).expect("small");
            let h = crate::session_store::test_handle(id);
            agent_in_front(&h, pgid, program, reader);
            let now = aterm_gui_timing::wall_ms();
            join_bound(&h, pgid, source);
            in_log(&h, |log| {
                let turn = open_turn(log, now - 900_000, now - 600_000);
                let step = open_tool(log, turn, now - 500_000);
                close_tool(log, step, now - 400_000);
            });
            let row = row_at(&h, now);
            assert_ne!(
                row["tools"], "-",
                "PRECONDITION: {program} {case}: a whole join counts: {row:?}"
            );
            tap_batch(&h, source, batch);
            let row = row_at(&h, now);
            assert_eq!(row["tools"], "-", "{program} {case}: {row:?}");
            assert_ne!(row["turns"], "-", "{program} {case}: {row:?}");
            tap_batch(&h, source, &another_file);
            let row = row_at(&h, now);
            assert_eq!(
                row["tools"], "-",
                "{program} {case}, then another file bound: {row:?}"
            );
        }
    }
    let h = crate::session_store::test_handle(49);
    agent_in_front(&h, 7299, "codex", aterm_phase::Program::Codex);
    let now = aterm_gui_timing::wall_ms();
    join_bound(&h, 7299, Source::Rollout);
    in_log(&h, |log| {
        let turn = open_turn(log, now - 900_000, now - 600_000);
        let step = open_tool(log, turn, now - 500_000);
        close_tool(log, step, now - 400_000);
    });
    assert_ne!(row_at(&h, now)["tools"], "-", "PRECONDITION: whole");
    h.ctx.timeline.lock().unwrap().slog_mut().rollout_stopped();
    let row = row_at(&h, now);
    assert_eq!(row["tools"], "-", "codex stopped: {row:?}");
    join_bound(&h, 7299, Source::Rollout);
    let row = row_at(&h, now);
    assert_ne!(row["tools"], "-", "codex reads again: {row:?}");
    let h = crate::session_store::test_handle(47);
    agent_in_front(&h, 7298, "claude", aterm_phase::Program::Claude);
    join_bound(&h, 7298, Source::Transcript);
    in_log(&h, |log| {
        let turn = open_turn(log, now - 900_000, now - 600_000);
        let step = open_tool(log, turn, now - 500_000);
        close_tool(log, step, now - 400_000);
    });
    assert_ne!(row_at(&h, now)["tools"], "-", "PRECONDITION: whole");
    crate::claude_footer::stop_session_taken(47, &h.ctx.timeline);
    let row = row_at(&h, now);
    assert_eq!(row["tools"], "-", "claude stopped: {row:?}");
    join_bound(&h, 7298, Source::Transcript);
    let row = row_at(&h, now);
    assert_ne!(row["tools"], "-", "claude reads again: {row:?}");
}

/// `n` tool steps the agent run open in `h`'s log (the newest one) takes in one turn, each
/// opened and closed before `now`.
fn agents_tools(h: &SessionHandle, n: u64, now: u64) {
    use aterm_slog::{Conf, Kind, Open, Src};
    in_log(h, |log| {
        let run = log
            .open_nodes()
            .iter()
            .rev()
            .find(|e| e.kind == Kind::Agent)
            .map(|e| e.node)
            .expect("an agent run open");
        let turn = log
            .open(
                Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
                wall(now - 900_000),
            )
            .expect("the turn");
        for i in 0..n {
            let step = open_tool(log, turn, now - 800_000 + i * 1_000);
            close_tool(log, step, now - 800_000 + i * 1_000 + 500);
        }
        close_turn(log, turn, now - 600_000);
    });
}

/// THE TOOL COUNT IS THE RUN IN FRONT'S: `tools=` counts the tool steps of the agent run in
/// front, as its transcript join holds them, never the session's. The join judged whole is that
/// run's own (each run has its own), so a session total after an earlier run lost rows would be
/// short yet sent as exact: a run that logged 2 then lost a row, then a run that logged 3, says
/// 3. A fresh run whose join is whole says its own count (0 before its first tool step), and a
/// run whose join is not whole says `-`.
/// CONTROL: the session's total sent for a whole join (`log.totals.tools`) is caught.
#[test]
fn the_tool_count_is_the_run_in_fronts_not_the_sessions() {
    use crate::session_log::transcript::Source;
    let h = crate::session_store::test_handle(50);
    let (pgid, claude) = (7301, aterm_phase::Program::Claude);
    let now = aterm_gui_timing::wall_ms();
    agent_in_front(&h, pgid, "claude", claude);
    join_bound(&h, pgid, Source::Transcript);
    assert_eq!(
        row_at(&h, now)["tools"],
        "0",
        "a fresh run, nothing run yet"
    );
    agents_tools(&h, 2, now);
    assert_eq!(row_at(&h, now)["tools"], "2", "a fresh run, its join whole");
    let lost = {
        use aterm_agent::harness::steps::{Step, StepBatch, StepEvent, StepPos, StepState};
        StepBatch {
            steps: vec![Step {
                pos: StepPos {
                    fp: 1,
                    id: None,
                    off: 1,
                    at_ms: 0,
                },
                state: StepState::Partial,
                ev: StepEvent::Lost { n: 1 },
            }],
            ..StepBatch::default()
        }
    };
    tap_batch(&h, Source::Transcript, &lost);
    assert_eq!(row_at(&h, now)["tools"], "-", "the first run lost a row");
    // The first run ends (the shell is back in front) and a second one starts.
    {
        let mut tl = h.ctx.timeline.lock().unwrap();
        let _ = tl.sweep_slog(&crate::session_log::BlockRead::default(), false, 100, 100);
        assert!(
            tl.slog().transcript_view(Source::Transcript).is_none(),
            "PRECONDITION: no agent run open"
        );
    }
    assert_eq!(row_at(&h, now)["tools"], "-", "no agent run open");
    agent_in_front(&h, pgid + 1, "claude", claude);
    join_bound(&h, pgid + 1, Source::Transcript);
    agents_tools(&h, 3, now);
    assert!(
        h.ctx
            .timeline
            .lock()
            .unwrap()
            .slog()
            .transcript_whole(claude),
        "PRECONDITION: the second run's join is whole"
    );
    let row = row_at(&h, now);
    assert_eq!(row["tools"], "3", "the second run's own count: {row:?}");
}

/// A HARNESS WITH NO TRANSCRIPT SAYS SCREEN AND NO TOKENS: agy's and Muse Code's rows take their
/// last output from the screen's last move (`out_src=screen`), whatever words or steps aterm
/// read of them, send no token counters (`-`, never `0`) and no tool count, and still say their
/// turn.
#[test]
fn a_harness_with_no_transcript_says_screen_and_no_tokens() {
    for (id, pgid, program, reader) in [
        (27, 7001, "agy", aterm_phase::Program::Agy),
        (28, 7002, "muse", aterm_phase::Program::Muse),
    ] {
        let h = crate::session_store::test_handle(id);
        agent_in_front(&h, pgid, program, reader);
        assert!(
            h.ctx
                .screen_moves
                .fed(crate::control::ScreenGen { epoch: 1, seq: 99 })
        );
        let now = aterm_gui_timing::wall_ms();
        let moved = h.ctx.screen_moves.moved_ms();
        {
            let mut tl = h.ctx.timeline.lock().unwrap();
            tl.set_claude_excerpts(
                pgid,
                Some(ClaudeExcerpts {
                    excerpts: Excerpts {
                        said: Some("Done.".into()),
                        said_at_ms: Some(i64::try_from(now - 300_000).unwrap()),
                        ..Excerpts::default()
                    },
                    ..ClaudeExcerpts::default()
                }),
            );
            tl.set_claude_footer(
                pgid,
                None,
                Some(usage_footer(claude_usage(true, 0, false), "conv-m")),
            );
        }
        in_log(&h, |log| {
            let turn = open_turn(log, now - 900_000, now - 600_000);
            let step = open_tool(log, turn, now - 500_000);
            close_tool(log, step, now - 400_000);
        });
        let row = row_at(&h, now);
        for (key, want) in [
            ("out_ms", (now - moved).to_string()),
            ("out_src", "screen".to_owned()),
            ("tok_in", "-".to_owned()),
            ("tok_out", "-".to_owned()),
            ("tok_for_ms", "-".to_owned()),
            ("tools", "-".to_owned()),
            ("turn", "1".to_owned()),
            ("turn_ms", "600000".to_owned()),
        ] {
            assert_eq!(row[key], want, "{program} {key}: {row:?}");
        }
        assert_ne!(row["turns"], "-", "{program}: {row:?}");
    }
}

/// A ROW THAT IS NO AGENT'S SENDS DASHES: a shell's row says nothing of a turn, a step, an
/// output or tokens (`-` for each of the eleven, never `turn=0`), whatever its log or screen
/// holds.
#[test]
fn a_row_that_is_no_agents_sends_dashes() {
    let h = crate::session_store::test_handle(29);
    {
        let mut tl = h.ctx.timeline.lock().unwrap();
        tl.note_foreground_group(9000);
        tl.set_program(9000, Some("zsh".into()));
    }
    assert!(
        h.ctx
            .screen_moves
            .fed(crate::control::ScreenGen { epoch: 1, seq: 99 })
    );
    let now = aterm_gui_timing::wall_ms();
    in_log(&h, |log| {
        let turn = open_turn(log, now - 900_000, now - 600_000);
        open_tool(log, turn, now - 500_000);
    });
    let row = row_at(&h, now);
    assert_eq!(row["program_kind"], "shell", "PRECONDITION: {row:?}");
    for key in PROGRESS_KEYS {
        assert_eq!(row[key], "-", "{key}: {row:?}");
    }
}

/// A SESSION ROW SAYS WHETHER IT IS PINNED, at `facts` as at `content`: `pinned=1` once a person
/// or a lead agent pinned it, `pinned=0` before and after. CONTROL: a row that does not read the
/// pin says 0 for a pinned session.
#[test]
fn a_session_row_says_whether_it_is_pinned() {
    let h = crate::session_store::test_handle(31);
    let now = aterm_gui_timing::wall_ms();
    let content = |h: &SessionHandle| {
        fields(
            &session_row(
                h,
                SurveyLevel::Content,
                None,
                (crate::metrics::now_us(), now, 0),
            )
            .text,
        )
    };
    assert_eq!(row_at(&h, now)["pinned"], "0");
    assert!(crate::session_timeline::set_pinned(
        &h.ctx,
        true,
        aterm_slog::Actor::Ctl
    ));
    assert_eq!(row_at(&h, now)["pinned"], "1");
    assert_eq!(content(&h)["pinned"], "1");
    assert!(crate::session_timeline::set_pinned(
        &h.ctx,
        false,
        aterm_slog::Actor::Ctl
    ));
    assert_eq!(row_at(&h, now)["pinned"], "0");
}

/// A row carries its harness upgrade word as `status` prints it (`upgrade=`), at facts and at
/// content, `-` where the host sent none, so a fleet read shows a held one (on 2026-11-13 five
/// tabs on one Mac stood at `pending/2.1.325/account` for hours and no fleet read could show it).
/// NEGATIVE CONTROL: a producer that writes `upgrade=-` whatever the timeline holds fails the
/// pending arm, and one that writes the kept row after the host sent none fails the cleared arm.
#[test]
fn a_rows_upgrade_word_is_the_one_status_prints() {
    use aterm_agent::harness::upgrade::Phase;
    use aterm_agent::harness::upgrade_drive::Row;
    let store = crate::session_store::new_store();
    let claude = crate::session_store::test_handle(5);
    claude_session(&claude);
    store.write().unwrap().register(claude.clone());
    let word_at = |level| {
        let reply = survey_rows(&store, level, usize::MAX, &here());
        fields(reply.lines().nth(2).expect("the claude row"))["upgrade"].clone()
    };
    for level in [SurveyLevel::Facts, SurveyLevel::Content] {
        assert_eq!(word_at(level), "-", "{level:?}: no upgrade row");
    }
    // Whole hours behind, so a second's skew between this read and the survey's leaves the age.
    let now = aterm_time::unix_secs().unwrap_or(0);
    let row = Row {
        tab: claude.sid.as_str().to_owned(),
        from: "2.1.324".into(),
        to: "2.1.325".into(),
        phase: Phase::Pending,
        behind_since: now - 2 * 3_600,
        wait: "account".into(),
        wait_since: now - 2 * 3_600,
        ..Row::default()
    };
    assert!(claude.ctx.timeline.lock().unwrap().set_upgrade(Some(row)));
    let (_, status) = claude
        .ctx
        .timeline
        .lock()
        .unwrap()
        .owner_columns(false, now);
    let printed = status
        .rsplit_once(" upgrade=")
        .map(|(_, word)| word.to_owned())
        .expect("status prints upgrade=");
    assert_eq!(printed, "pending/2.1.325/account/2h", "{status}");
    for level in [SurveyLevel::Facts, SurveyLevel::Content] {
        assert_eq!(word_at(level), printed, "{level:?}: the word status prints");
    }
    // A host that sends none leaves the word: the kept row is the top rule's, not a pending one.
    assert!(claude.ctx.timeline.lock().unwrap().set_upgrade(None));
    assert_eq!(word_at(SurveyLevel::Facts), "-", "cleared");
}
