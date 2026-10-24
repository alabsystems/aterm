// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn user(text: &str) -> String {
    let text = aterm_json::to_string(&Value::String(text.to_owned())).expect("json");
    format!(r#"{{"type":"user","message":{{"role":"user","content":{text}}}}}"#)
}

fn said(id: &str, text: &str) -> String {
    let text = aterm_json::to_string(&Value::String(text.to_owned())).expect("json");
    format!(
        r#"{{"type":"assistant","message":{{"id":"{id}","model":"claude-opus-5-5","content":[{{"type":"text","text":{text}}}]}}}}"#
    )
}

fn call(id: &str, tool: &str, name: &str) -> String {
    format!(
        r#"{{"type":"assistant","message":{{"id":"{id}","model":"claude-opus-5-5","content":[{{"type":"tool_use","id":"{tool}","name":"{name}","input":{{"command":"rm -rf /secret"}}}}]}}}}"#
    )
}

fn result(tool: &str) -> String {
    format!(
        r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"{tool}","content":[{{"type":"text","text":"output"}}]}}]}}}}"#
    )
}

/// `rows` (oldest first, as a transcript holds them) scanned.
fn scan(rows: &[String]) -> Scan {
    let newest_first: Vec<&str> = rows.iter().rev().map(String::as_str).collect();
    Scan::of(&newest_first)
}

fn excerpts(rows: &[String]) -> Excerpts {
    scan(rows).excerpts()
}

fn texts(e: &Excerpts) -> Vec<&str> {
    e.prompts.iter().map(|p| p.text.as_str()).collect()
}

/// Mid-call a tool is in flight by name (never its input), and once its result is written nothing is.
#[test]
fn a_call_is_in_flight_until_its_result_is_written() {
    let mut rows = vec![
        user("fix the footer"),
        said("m1", "Let me look at the tests."),
        call("m1", "t1", "Bash"),
    ];
    let mid = excerpts(&rows);
    assert_eq!(texts(&mid), ["fix the footer"]);
    assert_eq!(mid.said.as_deref(), Some("Let me look at the tests."));
    assert_eq!(mid.doing.as_deref(), Some("Bash"));
    assert!(
        !format!("{mid:?}").contains("secret"),
        "a tool's input is never read: {mid:?}"
    );
    rows.push(result("t1"));
    assert_eq!(excerpts(&rows).doing, None, "answered");
    rows.push(said("m2", "The footer is fixed."));
    let done = excerpts(&rows);
    assert_eq!(done.doing, None);
    assert_eq!(done.said.as_deref(), Some("The footer is fixed."));
    rows.push(user("now the tests"));
    let next = excerpts(&rows);
    assert_eq!(texts(&next), ["now the tests", "fix the footer"]);
    assert_eq!(next.doing, None);
}

/// Parallel calls stay in flight newest-first until each is answered.
#[test]
fn parallel_calls_stay_in_flight_until_each_is_answered() {
    let rows = vec![
        user("look around"),
        call("m1", "a", "Read"),
        call("m1", "b", "Grep"),
    ];
    assert_eq!(excerpts(&rows).doing.as_deref(), Some("Grep"));
    let mut b_done = rows.clone();
    b_done.push(result("b"));
    assert_eq!(excerpts(&b_done).doing.as_deref(), Some("Read"));
    let mut a_done = rows.clone();
    a_done.push(result("a"));
    assert_eq!(excerpts(&a_done).doing.as_deref(), Some("Grep"));
    let mut both = a_done;
    both.push(result("b"));
    assert_eq!(excerpts(&both).doing, None);
}

/// Folding scanned runs in any 2-way or 3-way split matches scanning the whole slice.
#[test]
fn any_split_reads_as_the_whole() {
    let rows = vec![
        user("first"),
        said("m1", "one"),
        call("m1", "a", "Read"),
        call("m1", "b", "Grep"),
        result("a"),
        user("keep going"),
        said("m2", "two"),
        call("m2", "c", "Bash"),
        call("m2", "d", "Edit"),
        result("d"),
        user("keep going"),
        said("m3", "three"),
        call("m3", "e", "Write"),
    ];
    for end in 1..=rows.len() {
        let whole = excerpts(&rows[..end]);
        for split in 0..=end {
            let older = scan(&rows[..split]);
            let newer = scan(&rows[split..end]);
            assert_eq!(
                newer.over(older).excerpts(),
                whole,
                "rows ..{end} split at {split}"
            );
            for first in 0..=split {
                let oldest = scan(&rows[..first]);
                let middle = scan(&rows[first..split]);
                let newest = scan(&rows[split..end]);
                assert_eq!(
                    newest.over(middle).over(oldest).excerpts(),
                    whole,
                    "rows ..{end} split at {first} and {split}"
                );
            }
        }
    }
}

/// Sidechains, synthetic/API-error replies, meta/compaction rows, interruptions, slash commands, and harness lines are ignored; `!` bash inputs count as prompts.
#[test]
fn only_typed_words_and_the_agents_own_text_count() {
    let rows = vec![
        user("the real prompt"),
        said("m1", "the real answer"),
        r#"{"type":"user","isSidechain":true,"message":{"content":"a subagent's task"}}"#.into(),
        r#"{"type":"assistant","isSidechain":true,"message":{"id":"s","model":"claude-opus-5-5","content":[{"type":"text","text":"a subagent's answer"}]}}"#.into(),
        r#"{"type":"user","isMeta":true,"message":{"content":"Caveat: local commands"}}"#.into(),
        r#"{"type":"user","isCompactSummary":true,"message":{"content":"This session is being continued"}}"#.into(),
        user("[Request interrupted by user]"),
        user("<command-name>/model</command-name>"),
        user("<local-command-stdout>Set model to Opus</local-command-stdout>"),
        user("[aterm harness] the live upgrade"),
        r#"{"type":"assistant","message":{"id":"x","model":"<synthetic>","content":[{"type":"text","text":"No response requested."}]}}"#.into(),
        r#"{"type":"assistant","isApiErrorMessage":true,"message":{"id":"y","model":"claude-opus-5-5","content":[{"type":"text","text":"API Error: 500"}]}}"#.into(),
    ];
    let e = excerpts(&rows);
    assert_eq!(texts(&e), ["the real prompt"]);
    assert_eq!(e.said.as_deref(), Some("the real answer"));
    let mut bang = rows;
    bang.push(user("<bash-input>git status</bash-input>"));
    assert_eq!(texts(&excerpts(&bang)), ["! git status", "the real prompt"]);
}

/// Prompts are deduplicated by normalized content key and kept newest-first up to `PROMPTS_KEPT`.
#[test]
fn prompts_are_distinct_newest_first_and_keyed_by_their_words() {
    let mut rows = vec![user("ship the survey")];
    for n in 0..20 {
        rows.push(said(&format!("m{n}"), "working"));
        rows.push(user("keep going"));
    }
    let e = excerpts(&rows);
    assert_eq!(texts(&e), ["keep going", "ship the survey"]);
    assert_eq!(e.prompts[0].key, prompt_key("keep going"));
    assert_eq!(e.prompts[1].key, prompt_key("ship  the\nsurvey "));
    let pasted = "\n\n<pasted_content id=\"1\">\nship the survey\n</pasted_content id=\"1\">\n";
    assert_eq!(prompt_key(pasted), prompt_key("ship the survey"));
    let many: Vec<String> = (0..20).map(|n| user(&format!("prompt {n}"))).collect();
    let e = excerpts(&many);
    assert_eq!(e.prompts.len(), PROMPTS_KEPT);
    assert_eq!(e.prompts[0].text, "prompt 19");
}

/// Every excerpt is folded to one line and capped at [`EXCERPT_BYTES`] on a character boundary with `…`.
#[test]
fn excerpts_are_one_line_and_capped() {
    let long = "été ".repeat(400);
    let e = excerpts(&[user(&long), said("m1", &format!("line one\n\n{long}"))]);
    for text in [e.prompts[0].text.as_str(), e.said.as_deref().unwrap_or("")] {
        assert!(text.len() <= EXCERPT_BYTES, "{}", text.len());
        assert!(text.ends_with('…'), "{text}");
        assert!(!text.contains('\n'), "{text}");
    }
    assert!(e.said.as_deref().unwrap_or("").starts_with("line one été"));
    assert_eq!(cap("short", 400), "short");
    assert_eq!(fold("  a \n\t b  "), "a b");
}

/// [`cap`] backs up out of a split ASCII word, yields `…` when one word exceeds the cap, and cuts CJK runs in place.
#[test]
fn the_cap_never_ends_inside_a_word() {
    assert_eq!(cap("alpha beta gamma", 12), "alpha…");
    assert_eq!(cap("alpha sk-proj-abc gamma", 18), "alpha…");
    assert_eq!(cap(&"x".repeat(600), 400), "…");
    assert_eq!(cap("alpha beta gamma", 13), "alpha beta…");
    let cjk = "漢字".repeat(100);
    let cut = cap(&cjk, 40);
    assert!(cut.len() > 30 && cut.len() <= 40, "{cut}");
    assert!(cjk.starts_with(cut.trim_end_matches('…')), "{cut}");
}

/// [`cap_end`] advances past a split leading ASCII word, yields `…` when one word exceeds the cap, and cuts CJK runs in place.
#[test]
fn the_end_cap_never_starts_inside_a_word() {
    assert_eq!(cap_end("alpha beta gamma", 12), "…gamma");
    assert_eq!(cap_end("alpha sk-proj-abc gamma", 12), "…gamma");
    assert_eq!(cap_end(&"x".repeat(600), 400), "…");
    assert_eq!(cap_end("alpha beta gamma", 14), "…beta gamma");
    assert_eq!(cap_end("short", 400), "short");
    let cjk = "漢字".repeat(100);
    let cut = cap_end(&cjk, 40);
    assert!(cut.len() > 30 && cut.len() <= 40, "{cut}");
    assert!(cjk.ends_with(cut.trim_start_matches('…')), "{cut}");
}

/// Neither [`cap`] nor [`cap_end`] splits a valid spaced fleet join code across the excerpt boundary, regardless of surrounding punctuation.
#[cfg(unix)]
#[test]
fn a_cut_never_falls_inside_a_join_code() {
    use aterm_net::fleet_member::code_spans;
    let code = aterm_net::fleet_member::JoinCode {
        secret: [0x5a; aterm_net::fleet_member::SECRET_LEN],
        host: "m7.local".to_owned(),
        port: 7100,
    }
    .text();
    let groups: Vec<String> = code["atf1-".len()..]
        .split('@')
        .next()
        .unwrap()
        .split('-')
        .map(str::to_ascii_uppercase)
        .collect();
    let spaced = |groups: &[String]| format!("ATF1- {}", groups.join(" "));
    let room = EXCERPT_BYTES - '…'.len_utf8();
    let filler = |len: usize| format!("{} ", "x".repeat(len - 1));
    let holds_a_group = |text: &str, groups: &[String]| {
        let mut left = String::new();
        let mut at = 0;
        for span in code_spans(text) {
            left.push_str(&text[at..span.start]);
            left.push(' ');
            at = span.end;
        }
        left.push_str(&text[at..]);
        let left = left.to_ascii_uppercase();
        left.contains("ATF1") || groups.iter().any(|g| left.contains(g.as_str()))
    };
    let typed = spaced(&groups);
    for (open, close) in [("", ""), ("`", "`"), ("", "."), ("(", ")."), ("\"", "\",")] {
        for shift in 0..=typed.len() {
            let text = format!(
                "{}{open}{typed}{close} and more",
                filler(room - shift - open.len())
            );
            assert_eq!(code_spans(&text).len(), 1, "{text}");
            let head = cap(&text, EXCERPT_BYTES);
            assert!(!holds_a_group(&head, &groups), "{open}{shift}: {head}");
            let text = format!(
                "more and {open}{typed}{close} {}",
                "x".repeat(room + shift - typed.len() - close.len() - 1)
            );
            assert_eq!(code_spans(&text).len(), 1, "{text}");
            let end = cap_end(&text, EXCERPT_BYTES);
            assert!(!holds_a_group(&end, &groups), "{open}{shift}: {end}");
        }
    }
    let text = format!("{}{typed} and more", filler(room - typed.len()));
    assert_eq!(code_spans(&cap(&text, EXCERPT_BYTES)).len(), 1);
    let text = format!("more and {typed} {}", "x".repeat(room - typed.len() - 1));
    assert_eq!(code_spans(&cap_end(&text, EXCERPT_BYTES)).len(), 1);
    let mut other = groups.clone();
    *other.last_mut().unwrap() = if other[8] == "ZZZZ" { "YYYY" } else { "ZZZZ" }.to_owned();
    let typed = spaced(&other);
    let shift = typed.len() / 2;
    let text = format!("{}{typed} and more", filler(room - shift));
    let head = cap(&text, EXCERPT_BYTES);
    assert!(head.contains(&format!("ATF1- {} ", other[0])), "{head}");
    assert!(holds_a_group(&head, &other), "{head}");
}

/// An answer longer than [`EXCERPT_BYTES`] keeps both its capped head ([`Excerpts::said`]) and tail ([`Excerpts::said_end`]).
#[test]
fn a_long_said_is_kept_by_its_head_and_by_its_end() {
    let report = format!(
        "Created notes.md on TCP slow start. {}`wc -w notes.md` printed **142**.",
        "It covers the congestion window and ssthresh. ".repeat(12)
    );
    assert!(report.len() > EXCERPT_BYTES);
    let e = excerpts(&[user("write notes.md"), said("m1", &report)]);
    let head = e.said.as_deref().unwrap_or_default();
    let end = e.said_end.as_deref().unwrap_or_default();
    assert!(
        head.starts_with("Created notes.md") && head.ends_with('…'),
        "{head}"
    );
    assert!(!head.contains("**142**"), "{head}");
    assert!(
        end.starts_with('…') && end.ends_with("printed **142**."),
        "{end}"
    );
    assert!(head.len() <= EXCERPT_BYTES && end.len() <= EXCERPT_BYTES);
    let short = excerpts(&[said("m1", "The footer is fixed.")]);
    assert_eq!(short.said_end, short.said);
    assert_eq!(short.said.as_deref(), Some("The footer is fixed."));
}

/// Standalone `ATERM-TASK-DONE` lines are stripped from `said`, falling back to the previous assistant message when nothing else remains.
#[test]
fn said_drops_the_done_tokens_lines_as_the_result_does() {
    let ended = excerpts(&[
        user("land the fix"),
        said("m1", "Merged the fix; the checks pass.\n\nATERM-TASK-DONE"),
    ]);
    assert_eq!(
        ended.said.as_deref(),
        Some("Merged the fix; the checks pass.")
    );

    let rows = [
        user("review the PR"),
        said("m1", "Finished the review: one real issue."),
        said("m2", "**ATERM-TASK-DONE**"),
    ];
    assert_eq!(
        excerpts(&rows).said.as_deref(),
        Some("Finished the review: one real issue.")
    );
    let newer = scan(&rows[2..]);
    assert_eq!(newer.excerpts().said, None);
    let joined = newer.over(scan(&rows[..2]));
    assert_eq!(
        joined.excerpts().said.as_deref(),
        Some("Finished the review: one real issue.")
    );

    let quoted = excerpts(&[
        user("how do workers end?"),
        said(
            "m1",
            "A worker ends with ATERM-TASK-DONE on a line by itself.",
        ),
    ]);
    assert_eq!(
        quoted.said.as_deref(),
        Some("A worker ends with ATERM-TASK-DONE on a line by itself.")
    );
}

/// Marked Autopilot continuation, done-check, land, and relaunch prompts are excluded from `prompts`, while unmarked user text is kept.
#[test]
fn the_harnesss_own_acts_are_no_prompt() {
    use crate::supervise::policy::turn_end::{DONE_CHECK, LAND, LAND_NUDGE};
    let carry_on =
        crate::harness::relaunch::resumed_prompt("2.1.300", crate::harness::relaunch::CAUSE_HOST);
    for act in [
        crate::supervise::config::CONTINUE_TEXT,
        DONE_CHECK,
        LAND,
        LAND_NUDGE,
        carry_on.as_str(),
    ] {
        let rows = vec![user("ship the survey"), said("m1", "working"), user(act)];
        assert_eq!(texts(&excerpts(&rows)), ["ship the survey"], "{act}");
    }
    let rows = vec![
        user("ship the survey"),
        said("m1", "working"),
        user("keep going"),
    ];
    assert_eq!(
        texts(&excerpts(&rows)),
        ["keep going", "ship the survey"],
        "CONTROL: the owner's unmarked words"
    );
}

#[test]
fn agy_excerpts_read_prompts_said_and_doing_from_transcript_and_history() {
    let cid = "dfaaf1c5-98ff-4412-9226-76412af952a5";
    let transcript = concat!(
        r#"{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","content":"<USER_REQUEST>\nhi\nwhat is next?\n</USER_REQUEST>\n<ADDITIONAL_METADATA>\ntime\n</ADDITIONAL_METADATA>"}"#,
        "\n",
        r#"{"step_index":1,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","content":"Working on the survey excerpts.","tool_calls":[{"name":"default_api:read_file","args":{}},{"name":"default_api:run_command","args":{}}]}"#,
        "\n",
        r#"{"step_index":2,"source":"MODEL","type":"GENERIC","status":"DONE","content":"read_file output"}"#,
        "\n",
        r#"{"step_index":3,"source":"MODEL","type":"GENERIC","status":"RUNNING","content":"Tool is running as a background task"}"#,
        "\n",
    );
    let history = concat!(
        r#"{"display":"earlier prompt","timestamp":1,"workspace":"/w","conversationId":"dfaaf1c5-98ff-4412-9226-76412af952a5"}"#,
        "\n",
        r#"{"display":"[aterm harness] Autopilot is Cruise: keep going.","timestamp":2,"workspace":"/w","conversationId":"dfaaf1c5-98ff-4412-9226-76412af952a5"}"#,
        "\n",
    );
    let got = agy_excerpts_from(transcript.as_bytes(), Some(history.as_bytes()), cid);
    assert_eq!(texts(&got), ["hi what is next?", "earlier prompt"]);
    assert_eq!(got.said.as_deref(), Some("Working on the survey excerpts."));
    assert_eq!(got.doing.as_deref(), Some("run_command"));

    let finished = format!(
        "{transcript}{}\n",
        r#"{"step_index":4,"source":"MODEL","type":"GENERIC","status":"DONE","content":"run_command output"}"#
    );
    let done = agy_excerpts_from(finished.as_bytes(), Some(history.as_bytes()), cid);
    assert_eq!(done.doing, None);
}

#[test]
fn muse_excerpts_read_prompts_said_and_doing_from_session_log() {
    let session = concat!(
        r#"{"retained_frame":"f","children":[{"record_json":"{\"payload_type\":\"runtime.session\",\"payload\":{\"kind\":\"run\",\"run_id\":\"r1\",\"event\":{\"kind\":\"started\",\"prompt\":\"write the landing report round 1\"}}}"}]}"#,
        "\n",
        r#"{"payload_type":"runtime.session","payload":{"kind":"run","run_id":"r1","event":{"kind":"assistant_message_committed","text":"Round 1 complete.\nAll checks passed."}}}"#,
        "\n",
        r#"{"payload_type":"runtime.session","payload":{"kind":"run","run_id":"r1","event":{"kind":"terminal","terminal":"completed"}}}"#,
        "\n",
        r#"{"payload_type":"runtime.session","payload":{"kind":"run","run_id":"r2","event":{"kind":"started","prompt":"[aterm harness] Autopilot is Cruise: keep going."}}}"#,
        "\n",
        r#"{"payload_type":"runtime.session","payload":{"kind":"task","run_id":"r2","task_id":"t0","event":{"kind":"proposed","task_kind":"session_name.allocate"}}}"#,
        "\n",
        r#"{"payload_type":"runtime.session","payload":{"kind":"task","run_id":"r2","task_id":"t1","event":{"kind":"proposed","task_kind":"tool.bash"}}}"#,
        "\n",
        r#"{"payload_type":"runtime.session","payload":{"kind":"task","run_id":"r2","task_id":"t0","event":{"kind":"completed"}}}"#,
        "\n",
        r#"{"payload_type":"runtime.session","payload":{"kind":"task","run_id":"r2","task_id":"t2","event":{"kind":"proposed","task_kind":"tool.read_file"}}}"#,
        "\n",
        r#"{"payload_type":"runtime.session","payload":{"kind":"task","run_id":"r2","task_id":"t2","event":{"kind":"completed"}}}"#,
        "\n",
    );
    let got = muse_excerpts_from(session.as_bytes());
    assert_eq!(texts(&got), ["write the landing report round 1"]);
    assert_eq!(
        got.said.as_deref(),
        Some("Round 1 complete. All checks passed.")
    );
    assert_eq!(got.doing.as_deref(), Some("bash"));

    let completed = format!(
        "{session}{}\n",
        r#"{"payload_type":"runtime.session","payload":{"kind":"task","run_id":"r2","task_id":"t1","event":{"kind":"completed"}}}"#
    );
    let done = muse_excerpts_from(completed.as_bytes());
    assert_eq!(done.doing, None);
}
