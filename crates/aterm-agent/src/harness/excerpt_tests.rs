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

/// A turn as Claude Code writes it: the prompt, a narration, a call, its
/// result, the answer. Mid-call the tool is in flight BY NAME (its input is
/// nowhere in the excerpts); once its result is written, nothing is.
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
    // A new prompt after the answer: nothing is in flight while it thinks.
    rows.push(user("now the tests"));
    let next = excerpts(&rows);
    assert_eq!(texts(&next), ["now the tests", "fix the footer"]);
    assert_eq!(next.doing, None);
}

/// Parallel calls of one message: each answered call leaves the others in
/// flight, the newest first.
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

/// The fold is exact: rows read as runs — an append over what was read, a
/// window walk's newest window over each older one in turn — say what the
/// whole says, at every row boundary.
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
            // Three runs, folded as the walk back folds them: the newest
            // over the middle, then that over the oldest.
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

/// None of the rows Claude Code writes itself is a prompt or an answer: a
/// sidechain, a `<synthetic>` or API-error answer, meta and compaction rows,
/// an Esc, a command and its output, the harness's own lines. A person's `!`
/// command is theirs.
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

/// Prompts are kept distinct by key and newest first, so the harness's one
/// repeated line costs one slot and the prompt under it is still there; a
/// paste is keyed by what was pasted, as the turn that typed it is.
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

/// Every excerpt is one line and at most [`EXCERPT_BYTES`], cut on a
/// character boundary with `…`; the key reads the whole prompt's head.
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

/// THE CAP NEVER ENDS INSIDE A WORD: a cut inside a run of ASCII letters,
/// digits and punctuation backs up to its start, so no token leaves cut
/// short; a run longer than the cap is `…` alone; a cut inside a run of CJK
/// stays where it falls. CONTROL: a cut that falls on a space keeps every
/// word before it.
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

/// THE END'S CAP NEVER STARTS INSIDE A WORD ([`cap_end`], [`cap`]'s rule from
/// the other end): a cut inside a run of ASCII letters, digits and punctuation
/// moves on to the run's end, so no token is quoted cut short; a run longer than
/// the cap is `…` alone; a cut inside a run of CJK stays where it falls.
/// CONTROL: a cut that falls on a space keeps every word after it.
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

/// A CUT NEVER FALLS INSIDE A FLEET JOIN CODE: a code typed with spaces
/// between its groups is nine words, and a cut between them would keep the
/// groups before it (or after it, from the end), each too short for the
/// survey's redaction to know as a code. Wherever the cut falls on the code,
/// its two edges included, and whatever is around it — spaces, or the
/// punctuation a report puts there (`` `…` ``, a `.`, `(…)`, `"…"`), which a
/// cut on an edge would back into the code by the never-inside-a-word rule —
/// what either excerpt keeps holds the code whole, where the redaction finds
/// it ([`code_spans`]), or none of its groups. NEGATIVE CONTROL: the same
/// groups with their check group changed are no code, and the cut keeps
/// every whole word before it, groups included, outside any code.
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
    // `len` bytes of text, a space last.
    let filler = |len: usize| format!("{} ", "x".repeat(len - 1));
    // Whether what the redaction leaves of `text` — its words outside every
    // code it finds — holds the prefix or a group.
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
            // The head's cut falls `shift` bytes into the code.
            let text = format!(
                "{}{open}{typed}{close} and more",
                filler(room - shift - open.len())
            );
            assert_eq!(code_spans(&text).len(), 1, "{text}");
            let head = cap(&text, EXCERPT_BYTES);
            assert!(!holds_a_group(&head, &groups), "{open}{shift}: {head}");
            // The end's cut falls `shift` bytes into the code.
            let text = format!(
                "more and {open}{typed}{close} {}",
                "x".repeat(room + shift - typed.len() - close.len() - 1)
            );
            assert_eq!(code_spans(&text).len(), 1, "{text}");
            let end = cap_end(&text, EXCERPT_BYTES);
            assert!(!holds_a_group(&end, &groups), "{open}{shift}: {end}");
        }
    }
    // A cut on an edge with a space beside it keeps the code whole, and the
    // redaction finds it there.
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

/// A REPORT PAST THE CAP IS KEPT BOTH WAYS: by its head
/// ([`Excerpts::said`]) and by its end ([`Excerpts::said_end`]), each within
/// [`EXCERPT_BYTES`]; only the end carries the result the head cut off.
/// CONTROL: a report within the cap is the same text both ways.
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

/// SAID WITHOUT THE DONE TOKEN, as the task's result reads a report: a line
/// that is `ATERM-TASK-DONE` alone (Markdown punctuation allowed) is dropped,
/// and a message that is the token alone gives the newest one with words —
/// in one read and across a read that ended between the two rows.
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
    // The token's row alone in the newer read: the older read says it.
    let newer = scan(&rows[2..]);
    assert_eq!(newer.excerpts().said, None);
    let joined = newer.over(scan(&rows[..2]));
    assert_eq!(
        joined.excerpts().said.as_deref(),
        Some("Finished the review: one real issue.")
    );

    // The CONTROL: the token inside a sentence is words, kept whole.
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

/// THE HARNESS'S OWN ACTS ARE NO PROMPT (Autopilot Cruise / Land):
/// Cruise's continuation (`CONTINUE_TEXT`, the default `continue_text`), its
/// prompt at a done claim (`DONE_CHECK`), Land's instruction and nudge, and a
/// restart's carry-on each open with the harness's mark, so the survey's
/// newest prompt stays the one a person or a manager typed. NEGATIVE
/// CONTROL: the owner's own unmarked `continue_text` (`keep going`) is a
/// prompt, which the survey skips only by the host's turn ledger.
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
