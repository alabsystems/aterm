// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_agent::harness::excerpt::Prompt;
use aterm_agent::harness::footer::FooterFacts;

/// The rows' fields, `key=value` split at the first `=`, values decoded.
fn fields(row: &str) -> std::collections::HashMap<String, String> {
    row.split_whitespace()
        .skip(1)
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.to_owned(), aterm_codec::pct::decode(v)))
        .collect()
}

/// This machine as a test's survey header names it: a fixed name and one
/// instance, whatever runs on the machine the test runs on.
fn here() -> Here {
    Here {
        host: Some("m7.local".to_owned()),
        instances: 1,
        drive: false,
    }
}

/// `reply` with every age (`<key>_ms=<n>`, `at_ms=` too) masked: two reads a
/// millisecond apart differ there and nowhere else.
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

/// One prompt as the footer's tail read keeps it.
fn prompt(text: &str) -> Prompt {
    Prompt {
        text: text.to_owned(),
        key: excerpt::prompt_key(text),
    }
}

/// A ledger record of `text` typed `by`.
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

/// A Claude Code session as the sweep and the footer resolver publish one:
/// busy, two shells counted, its footer facts, its excerpts — the newest
/// prompt the harness's `keep going` — and a task record.
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

/// THE VERB OVER STUB SESSIONS: one header, then a row per session in
/// local-id order.
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

/// A tasked worker's prompt is its task's head; a session with no task
/// record keeps the transcript's prompt even when a task head was read.
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

/// A FINISHED TURN'S `said=` IS SENT KEEPING ITS END, where its report puts the
/// result: its task `done` (or `exited`), or its agent `idle`.
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

/// The newest prompt the harness did not type, by key: whitespace and a
/// paste wrapper do not hide a harness text.
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

/// A fleet code is never quoted, a credential-shaped word is replaced, and a
/// line the privacy check finds sensitive is withheld whole; ordinary words,
/// short commit ids and line breaks are kept.
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
            host: "m7.local".to_owned(),
            port: 7100,
        }
        .text();
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
    aterm_net::fleet_member::write_key_file(dir.path(), &secret).unwrap();
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

/// A SECRET A SOFT WRAP SPLIT is judged whole: a shell's last rows are its
/// logical lines, each joined across the wrap (and its head from the
/// scrollback) before the redaction, which judges a line at a time — a
/// `password=` cut at the wrap column is withheld, and a 40-digit hex key in
/// a pane narrower than it is cut to a commit id's short form.
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

/// A SECRET AT THE EXCERPT CAP is never quoted in part. The harness keeps
/// `said` capped at [`excerpt::EXCERPT_BYTES`] as it reads it, before the
/// survey's redaction, and the cap ends before a word it would cut (a
/// finished turn's, kept by its end, starts after one): a
/// 40-character mixed-case token straddling byte 400 leaves none of its
/// characters in the row, where its first 17 — under the credential rule's
/// 32 — would read as no credential.
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

/// A BRANCH is a name its repository publishes: kept whole, however long or
/// token-like, or whatever words it holds — only a fleet code in it is
/// replaced, and it is capped.
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

/// A task's head is read only from a plain path under `<state>/tasks/` — on
/// Windows one on a drive (`C:\…`), as the state dir is.
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

/// A REMOTE HOLD NAMES ITS NODE on the facts row (`held_from=`), with an
/// Owner client's hold standing beside it: the Mac that placed it reads its
/// Hand Back by that node.
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

/// WHICH TAB RANG, read from the survey: the row of a session that rang
/// counts its bells and names the program in front of each, as `status`
/// does, at facts and at content alike.
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

/// WHAT EACH LEVEL CARRIES, field by field and in order: the facts are words,
/// counts and ages — attention only as a flag — and content adds the
/// excerpts and the attention line's words.
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
        "detail",
        "doing",
        "role",
        "supervisor",
        "attention",
        "needs",
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

/// WHO TYPED AT FACTS IS NEVER A TAB'S TEXT (S9): a typist tab the person
/// named `acme-renewal` reads as its session id at facts and by that name at
/// content; a role is a label facts carry.
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

/// THE EXCERPT IS THE ENTRY THE SESSION IS RANKED BY: a session whose
/// published need is an older ask, under a newer wait, shows the ask's words
/// at content — what the reader is asked to decide — and the facts row still
/// carries a flag alone.
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

/// THE REPLY'S CAP: a survey past `max` bytes answers its least pressing
/// sessions at facts — shells before agents, the newest first — counted in
/// `cut=`, and past that leaves their rows out, `sessions=` still the whole
/// count; it never passes `max` while a row is left to drop.
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

/// A ROLE IS A LABEL AT FACTS: a role that is one short word
/// (`agent:reviewer`) is sent at both levels as written; a role that is
/// text a person or an agent wrote — a space in it, or longer than a label —
/// is `-` at facts, which carry no such text, and at content an excerpt,
/// redacted (a credential-shaped word in it replaced).
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

/// THE HEADER SAYS WHAT `here` SAYS OF THIS MACHINE: the name its LAN
/// reaches it by, and how many aterm instances run here — a fleet reader
/// reads this one alone.
#[test]
fn the_header_names_this_machine_and_its_instances() {
    let store = crate::session_store::new_store();
    let two = Here {
        host: Some("Andrews-MacBook-Pro.local".to_owned()),
        instances: 2,
        drive: false,
    };
    let out = survey_rows(&store, SurveyLevel::Facts, usize::MAX, &two);
    let head = fields(out.lines().nth(1).unwrap());
    assert_eq!(head["host"], "Andrews-MacBook-Pro.local");
    assert_eq!(head["instances"], "2");
    let none = Here {
        host: None,
        instances: 1,
        drive: false,
    };
    let out = survey_rows(&store, SurveyLevel::Facts, usize::MAX, &none);
    let head = fields(out.lines().nth(1).unwrap());
    assert_eq!(
        (head["host"].as_str(), head["instances"].as_str()),
        ("-", "1")
    );
}

/// AN AGENT NO TRANSCRIPT SPEAKS FOR IS READ OFF ITS SCREEN: a Gemini CLI
/// (as a Muse Code, agy or Codex: only Claude Code's footer publishes
/// excerpts) is an agent (`program_kind=agent`, never a shell), and its
/// content row carries its command line and its last rows, redacted as a
/// shell's are.
#[test]
fn an_agent_with_no_excerpts_shows_its_last_rows() {
    let gemini = crate::session_store::test_handle(4);
    {
        const PGID: i32 = 4343;
        let mut tl = gemini.ctx.timeline.lock().unwrap();
        tl.note_foreground_group(PGID);
        tl.set_program(PGID, Some("gemini".into()));
        tl.publish_agent(
            "idle",
            None,
            None,
            Some(aterm_phase::Program::Gemini),
            crate::session_timeline::AgentStamp {
                generation: crate::control::ScreenGen { epoch: 1, seq: 3 },
                fp: 0x6e,
            },
        );
    }
    gemini
        .term
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
    store.write().unwrap().register(gemini);
    store.write().unwrap().register(claude);

    let out = survey_rows(&store, SurveyLevel::Content, usize::MAX, &here());
    let lines: Vec<&str> = out.lines().collect();
    let (gemini_row, claude_row) = (fields(lines[2]), fields(lines[3]));
    assert_eq!(gemini_row["program"], "gemini", "{out}");
    assert_eq!(gemini_row["program_kind"], "agent", "{out}");
    assert!(
        gemini_row["rows"].contains("The build is green."),
        "{}",
        lines[2]
    );
    assert!(!gemini_row["rows"].contains("hunter2"), "{}", lines[2]);
    assert_eq!(claude_row["said"], "Running the tests now.", "{out}");
    assert_eq!(claude_row["rows"], "-", "{}", lines[3]);
}

/// A frame the program has not finished gives `rows=-`, never its prefix, and no wait for the rest.
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

/// `program_kind=` is the server's word for what runs in front, so a reader
/// keeps no list of shells.
#[test]
fn the_program_kind_names_a_shell() {
    assert_eq!(program_kind(true, Some("claude")), "agent");
    assert_eq!(program_kind(false, Some("-zsh")), "shell");
    assert_eq!(program_kind(false, Some("fish")), "shell");
    assert_eq!(program_kind(false, Some("nvim")), "program");
    assert_eq!(program_kind(false, None), "-");
}
