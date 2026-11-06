// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for [`super`]: every refusal class on synthetic inputs (KEY on the version line and the
//! rows, STEM on the names, TRANSCRIPTS, TEXT, and NEW-KEY and GONE through the shared judge),
//! the step testdata read only as HEAD holds it, the session log judged as an unmeasured and a
//! measured build write it, the standing tests, and every Claude Code build in the step testdata
//! read as measured.

use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::process::ExitStatusExt as _;
use std::process::ExitStatus;

use super::*;
use crate::measure::{committed, header_build, read_but_not_held, repo_with, standing_verdict};

/// The step testdata, in the agent crate beside this package.
fn committed_dir() -> PathBuf {
    aterm_tempfile::manifest_dir().join("../aterm-agent/src/harness/testdata/steps")
}

const B: &str = "2.1.399";

fn reduction(name: &str, build: Option<&str>, text: &str) -> Reduction {
    Reduction {
        name: name.to_string(),
        build: build.map(str::to_string),
        text: text.to_string(),
    }
}

fn header(build: &str) -> String {
    reduce::header(Program::Claude, Some(build), 0, 1, true)
}

/// A transcript of one turn of `build`: the prompt (carrying [`WORD`]), the reply, its end.
fn transcript(build: &str) -> String {
    [
        format!(
            r#"{{"parentUuid":null,"isSidechain":false,"promptId":"7d0e3c1a-5b1f-4d7e-9a51-0c6f3e1a2b01","type":"user","message":{{"role":"user","content":"say hello {WORD}"}},"uuid":"0b1e7c42-1d6f-4b8e-a0d2-6f1c9e3a5b02","timestamp":"2026-10-28T10:00:00.000Z","version":"{build}"}}"#
        ),
        format!(
            r#"{{"parentUuid":"0b1e7c42-1d6f-4b8e-a0d2-6f1c9e3a5b02","isSidechain":false,"type":"assistant","message":{{"id":"msg_lab0001","model":"claude-opus-5-5","role":"assistant","content":[{{"type":"text","text":"Hello from the stub, plainly"}}],"usage":{{"input_tokens":3,"output_tokens":5}}}},"uuid":"2c4f8a10-7e3b-4c5d-9f60-1a2b3c4d5e03","timestamp":"2026-10-28T10:00:01.000Z","version":"{build}"}}"#
        ),
        format!(
            r#"{{"parentUuid":"2c4f8a10-7e3b-4c5d-9f60-1a2b3c4d5e03","isSidechain":false,"type":"system","subtype":"turn_duration","durationMs":1000,"isMeta":false,"uuid":"3d5a9b21-8f4c-4d6e-a071-2b3c4d5e6f04","timestamp":"2026-10-28T10:00:01.100Z","version":"{build}"}}"#
        ),
    ]
    .join("\n")
        + "\n"
}

/// A reducer that keeps every row as written under its own line `line`: a fixed point that keeps
/// all the text a real reduction replaces.
fn names(line: String) -> impl Fn(&str) -> Result<String, String> {
    move |t: &str| {
        Ok(format!(
            "{line}\n{}\n",
            rows(t).collect::<Vec<_>>().join("\n")
        ))
    }
}

#[test]
fn the_build_is_the_first_word_its_version_line_says() {
    assert_eq!(
        version_of("2.1.312 (Claude Code)\n").as_deref(),
        Some("2.1.312")
    );
    assert_eq!(version_of("2.1.312").as_deref(), Some("2.1.312"));
    for not in [
        "",
        "Claude Code 2.1.312",
        "v2.1.312 (Claude Code)",
        "2.1.x (Claude Code)",
        "2.1.312. (Claude Code)",
        "\n2.1.312",
    ] {
        assert_eq!(version_of(not), None, "{not:?}");
    }
    let tmp = aterm_tempfile::tempdir().expect("a scratch dir");
    let stand_in = |name: &str, body: &str| {
        let p = tmp.path().join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).expect("a stand-in");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        p.to_string_lossy().into_owned()
    };
    let ok = stand_in("claude", "echo '2.1.399 (Claude Code)'");
    assert_eq!(installed_build(&ok).map_err(|r| r.why), Ok(B.to_string()));
    for (name, body, said) in [
        ("words", "echo 'Claude Code'", "said no build"),
        ("fails", "echo '2.1.399 (Claude Code)'; exit 3", "exited"),
    ] {
        let r = installed_build(&stand_in(name, body)).expect_err(name);
        assert_eq!(r.class, Class::Key, "{}", r.why);
        assert!(r.why.contains(said), "{}", r.why);
    }
    let gone = tmp.path().join("none").to_string_lossy().into_owned();
    let r = installed_build(&gone).expect_err("no program");
    assert_eq!(r.class, Class::Key, "{}", r.why);
}

#[test]
fn a_measured_build_is_left_alone_and_a_taken_name_refuses_stem() {
    let measured = [reduction(
        "2.1.303.jsonl",
        Some("2.1.303"),
        &header("2.1.303"),
    )];
    assert_eq!(admit("2.1.303", &measured).map_err(|r| r.why), Ok(None));
    assert_eq!(
        admit(B, &measured).map_err(|r| r.why),
        Ok(Some("2.1.399.jsonl".to_string()))
    );
    for (taken, said) in [
        (
            reduction("2.1.399.jsonl", Some("2.1.398"), ""),
            "holds 2.1.398's",
        ),
        (reduction("2.1.399.jsonl", None, ""), "holds no build's"),
        (reduction("x.jsonl", Some(B), ""), "x.jsonl name 2.1.399"),
    ] {
        let r = admit(B, &[taken]).expect_err(said);
        assert_eq!(r.class, Class::Stem, "{}", r.why);
        assert!(r.why.contains(said), "{}", r.why);
    }
}

#[test]
fn a_transcript_row_that_names_another_build_refuses_key() {
    let source = transcript(B);
    key_holds(LEG, &source, B).expect("every row names the build");
    let mixed = source.replacen(
        &format!("\"version\":\"{B}\""),
        "\"version\":\"2.1.398\"",
        2,
    );
    let r = key_holds(LEG, &mixed, B).expect_err("a row names another");
    assert_eq!(r.class, Class::Key, "{}", r.why);
    assert!(
        r.why.contains("row 1") && r.why.contains("2.1.398"),
        "{}",
        r.why
    );
    let r = key_holds(LEG, r#"{"type":"mode","mode":""}"#, B).expect_err("no row names one");
    assert_eq!(r.class, Class::Key, "{}", r.why);
    let text = reduce_leg(&CLAUDE, LEG, &source, B, &names(header(B))).expect("its line names B");
    assert_eq!(header_build(Program::Claude, &text), Some(B));
    let r =
        reduce_leg(&CLAUDE, LEG, &source, B, &names(header("2.1.398"))).expect_err("another build");
    assert_eq!(r.class, Class::Key, "{}", r.why);
    assert!(r.why.contains("own line names 2.1.398"), "{}", r.why);
    let r = reduce_leg(
        &CLAUDE,
        LEG,
        &source,
        B,
        &names("# claude - · lab reduce".into()),
    )
    .expect_err("no build");
    assert_eq!(r.class, Class::Key, "{}", r.why);
    assert!(r.why.contains("no build"), "{}", r.why);
}

#[test]
fn a_session_with_no_transcript_or_two_refuses_transcripts() {
    let tmp = aterm_tempfile::tempdir().expect("a scratch dir");
    // The walk follows no link, as the lab's own scratch has none (`/var` is one).
    let root = std::fs::canonicalize(tmp.path()).expect("its real path");
    let r = one_transcript(&root).expect_err("none");
    assert_eq!(r.class, Class::Transcripts, "{}", r.why);
    let project = root.join("projects/-p");
    std::fs::create_dir_all(&project).expect("a project");
    std::fs::write(project.join("a.jsonl"), transcript(B)).expect("a transcript");
    std::fs::write(project.join("notes.txt"), "no transcript").expect("a file");
    assert_eq!(
        one_transcript(&root).map_err(|r| r.why),
        Ok(project.join("a.jsonl"))
    );
    std::fs::write(project.join("b.jsonl"), transcript(B)).expect("a second");
    let r = one_transcript(&root).expect_err("two");
    assert_eq!(r.class, Class::Transcripts, "{}", r.why);
    assert!(r.why.contains("2 transcripts"), "{}", r.why);
}

#[test]
fn a_reduction_that_keeps_text_refuses_text() {
    let source = transcript(B);
    let text = reduce_session(&source, B, &reduce_checked(Program::Claude)).expect("reduced");
    assert!(!text.contains(WORD) && !text.contains("plainly"), "{text}");
    assert_eq!(header_build(Program::Claude, &text), Some(B));
    let first = reduce::reduce(&source, Program::Claude).expect("pass 1");
    text_kept(&source, &first, &text).expect("no text kept");
    let r = text_kept(&source, &first, &format!("{text}{WORD}\n")).expect_err("the word kept");
    assert_eq!(r.class, Class::Text, "{}", r.why);
    assert!(r.why.contains("own word"), "{}", r.why);
    let prompt = format!("say hello {WORD}");
    let kept = reduce::Reduction {
        text: first
            .text
            .replacen("\"content\":\"\"", &format!("\"content\":\"{prompt}\""), 1),
        ..first.clone()
    };
    assert!(kept.text.contains(&prompt), "the prompt planted back");
    let r = text_kept(&source, &kept, &text).expect_err("the oracle finds the prompt");
    assert_eq!(r.class, Class::Text, "{}", r.why);
    let r = reduce_session(&source, B, &names(header(B))).expect_err("step 4 holds what it wrote");
    assert_eq!(r.class, Class::Text, "{}", r.why);
    assert!(r.why.contains("own word"), "{}", r.why);
}

/// The measured 2.1.303 a scratch reduction is judged against: a prompt, a reply and a turn's end.
fn measured() -> (String, [Reduction; 1]) {
    let old = format!(
        "{}\n{}\n{}\n{}\n",
        header("2.1.303"),
        r#"{"type":"user","parentUuid":null,"message":{"role":"user","content":""},"version":"2.1.303"}"#,
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":""}]},"version":"2.1.303"}"#,
        r#"{"type":"system","subtype":"turn_duration","durationMs":3,"isMeta":false,"version":"2.1.303"}"#
    );
    let committed = [reduction("2.1.303.jsonl", Some("2.1.303"), &old)];
    (old.replace("2.1.303", B), committed)
}

/// `same` with `rows` added.
fn with_rows(same: &str, rows: &[&str]) -> Vec<(&'static str, String)> {
    vec![(LEG, format!("{same}{}\n", rows.join("\n")))]
}

/// THE JUDGE'S RULE (a): a new key and a new record kind on rows the fold does not act on (an
/// `attachment`, a `tools-used`), a key the fold reads among them, are noted, and nothing waits
/// on run 2; the reducer's key names are no new key.
#[test]
fn a_new_key_on_a_row_the_fold_does_not_act_on_is_noted() {
    let (same, committed) = measured();
    let j = judge(&CLAUDE, B, &[(LEG, same.clone())], &committed).expect("nothing new");
    assert_eq!((j.builds, j.prev.as_deref()), (1, Some("2.1.303")));
    assert!(j.noted.is_empty() && j.pending.is_empty(), "{j:?}");
    let inert = with_rows(
        &same,
        &[
            r#"{"type":"attachment","attachment":{"type":"deferred_tools_delta","addedNames":[]},"scheduledTaskId":"t"}"#,
            r#"{"type":"tools-used","tools":[]}"#,
        ],
    );
    let j = judge(&CLAUDE, B, &inert, &committed).expect("only noted");
    assert!(j.pending.is_empty(), "{:?}", j.pending);
    for said in [
        "session: key \"addedNames\" on attachment/-, rows the fold does not act on",
        "session: key \"scheduledTaskId\" on attachment/-, rows the fold does not act on",
        "session: record attachment/-, a row the fold does not act on",
        "session: record tools-used/-, a row the fold does not act on",
    ] {
        assert!(j.noted.iter().any(|n| n == said), "{said}: {:?}", j.noted);
    }
    let renamed = with_rows(
        &same,
        &[r#"{"type":"system","subtype":"turn_duration","k9":{"k10":1}}"#],
    );
    let j =
        judge(&CLAUDE, B, &renamed, &committed).expect("the reducer's key names are no new key");
    assert!(j.noted.is_empty() && j.pending.is_empty(), "{j:?}");
}

/// THE JUDGE'S RULE (b), admitted: a new key on a row the fold acts on (a user row's `sandboxed`),
/// and a new record kind of one (`system/api_retry`), wait on run 2; run 2's reduction carrying
/// them admits each, naming the run, the row and the check that read it.
#[test]
fn a_new_key_on_a_row_the_fold_acts_on_is_admitted_once_run_2_exercised_it() {
    let (same, committed) = measured();
    let rows = [
        r#"{"type":"user","sandboxed":false,"message":{"role":"user","content":""}}"#,
        r#"{"type":"system","subtype":"api_retry","isMeta":false}"#,
    ];
    let acted = with_rows(&same, &rows);
    let j = judge(&CLAUDE, B, &acted, &committed).expect("left to run 2");
    let said: Vec<String> = j
        .pending
        .iter()
        .map(crate::measure::Pending::said)
        .collect();
    assert_eq!(
        said,
        [
            "session: key \"sandboxed\" on user/-",
            "session: record system/api_retry"
        ]
    );
    let admitted = exercised(&CLAUDE, B, &j.pending, RUN2, &acted, &|leg, _, row| {
        format!("{leg} row {row} read")
    })
    .expect("run 2 carries both");
    assert_eq!(
        admitted,
        [
            "session: key \"sandboxed\" on user/-: run 2 (expecting `on`), row 4 (session row 3 read)",
            "session: record system/api_retry: run 2 (expecting `on`), row 5 (session row 4 read)"
        ]
    );
}

/// THE JUDGE'S RULE (b), refused: a new key on a row the fold acts on that run 2's reduction
/// carries on no row of that kind (here only on an attachment) refuses NEW-KEY, naming the key,
/// its row kind and that no run exercised it.
#[test]
fn a_new_key_on_a_row_the_fold_acts_on_that_no_run_exercised_refuses_new_key() {
    let (same, committed) = measured();
    let acted = with_rows(
        &same,
        &[
            r#"{"type":"user","toolDenialKind":"user-rejected","message":{"role":"user","content":""}}"#,
        ],
    );
    let j = judge(&CLAUDE, B, &acted, &committed).expect("left to run 2");
    assert_eq!(j.pending.len(), 1, "{:?}", j.pending);
    let run2 = with_rows(
        &same,
        &[r#"{"type":"attachment","toolDenialKind":"user-rejected"}"#],
    );
    let r = exercised(&CLAUDE, B, &j.pending, RUN2, &run2, &|_, _, _| {
        String::new()
    })
    .expect_err("no run 2 user row carries it");
    assert_eq!(r.class, Class::NewKey, "{}", r.why);
    assert!(
        r.why.contains("key \"toolDenialKind\" on user/-")
            && r.why.contains("no run exercised it")
            && r.why.contains("wrote no user/- row carrying it"),
        "{}",
        r.why
    );
}

/// THE JUDGE'S RULE (c): a new key the fold reads (`TRANSCRIPT_KEYS`) on a row it acts on still
/// refuses NEW-KEY at step 5, as a format change the fold must be taught.
#[test]
fn a_new_key_the_fold_reads_refuses_new_key() {
    let (same, committed) = measured();
    let read = with_rows(
        &same,
        &[
            r#"{"type":"assistant","apiErrorStatus":429,"message":{"role":"assistant","content":[]}}"#,
        ],
    );
    let r = judge(&CLAUDE, B, &read, &committed).expect_err("a key the fold reads");
    assert_eq!(r.class, Class::NewKey, "{}", r.why);
    assert!(
        r.why
            .contains("key \"apiErrorStatus\" on assistant/-, a key the fold reads"),
        "{}",
        r.why
    );
}

/// THE JUDGE'S RULE holds a key by the row kinds a measured build showed it on, not by its name: a
/// key a measured build showed only on a row the fold does not act on (2.1.398's attachment, where
/// it was noted) is new to the fold the first time a build writes it on a row the fold acts on, so
/// one the fold reads refuses NEW-KEY (rule (c)) and any other waits on run 2 (rule (b)); and GONE
/// never holds a build to a key the previous build showed only on such a row.
#[test]
fn a_key_noted_on_an_inert_row_is_new_where_the_fold_acts() {
    let (same, [old]) = measured();
    let attachment = r#"{"type":"attachment","attachment":{"type":"deferred_tools_delta","addedNames":[]},"scheduledTaskId":"t"}"#;
    let noted = format!("{}{attachment}\n", same.replace(B, "2.1.398"));
    let j = judge(&CLAUDE, "2.1.398", &[(LEG, noted.clone())], &[old]).expect("only noted");
    assert!(j.pending.is_empty(), "{:?}", j.pending);
    let (_, [old]) = measured();
    let committed = [old, reduction("2.1.398.jsonl", Some("2.1.398"), &noted)];
    let same = noted.replace("2.1.398", B);
    let read_on_user = with_rows(
        &same,
        &[r#"{"type":"user","scheduledTaskId":"t","message":{"role":"user","content":""}}"#],
    );
    let r = judge(&CLAUDE, B, &read_on_user, &committed)
        .expect_err("a key the fold reads, new where the fold acts");
    assert_eq!(r.class, Class::NewKey, "{}", r.why);
    assert!(
        r.why.contains(
            "key \"scheduledTaskId\" on user/-, a key the fold reads, shown before only on rows the fold does not act on"
        ),
        "{}",
        r.why
    );
    let other_on_user = with_rows(
        &same,
        &[r#"{"type":"user","addedNames":[],"message":{"role":"user","content":""}}"#],
    );
    let j = judge(&CLAUDE, B, &other_on_user, &committed).expect("left to run 2");
    let said: Vec<String> = j
        .pending
        .iter()
        .map(crate::measure::Pending::said)
        .collect();
    assert_eq!(said, ["session: key \"addedNames\" on user/-"]);
    let dropped: String = same
        .lines()
        .filter(|l| !l.contains("attachment"))
        .map(|l| format!("{l}\n"))
        .collect();
    let j = judge(&CLAUDE, B, &[(LEG, dropped)], &committed)
        .expect("GONE holds no key the previous build showed only where the fold does not act");
    assert!(
        j.noted
            .iter()
            .any(|n| n == "session: key \"scheduledTaskId\" gone since 2.1.398"),
        "{:?}",
        j.noted
    );
}

#[test]
fn a_transcript_key_the_previous_build_showed_refuses_gone() {
    let old = format!(
        "{}\n{}\n",
        header("2.1.303"),
        r#"{"type":"system","subtype":"turn_duration","durationMs":3,"isMeta":false,"extra":1,"version":"2.1.303"}"#
    );
    let committed = [reduction("2.1.303.jsonl", Some("2.1.303"), &old)];
    let now = old.replace("2.1.303", B);
    let lost = vec![(LEG, now.replace(",\"isMeta\":false", ""))];
    let r = judge(&CLAUDE, B, &lost, &committed).expect_err("a key the fold reads is gone");
    assert_eq!(r.class, Class::Gone, "{}", r.why);
    assert!(
        r.why.contains("lost a transcript key the fold reads")
            && r.why.contains("transcript.isMeta"),
        "{}",
        r.why
    );
    let unread = vec![(LEG, now.replace(",\"extra\":1", ""))];
    let j = judge(&CLAUDE, B, &unread, &committed).expect("a key no reader reads may go");
    assert_eq!(j.noted, ["session: key \"extra\" gone since 2.1.303"]);
}

#[test]
fn the_step_testdata_is_read_only_as_head_holds_it() {
    let tmp = aterm_tempfile::tempdir().expect("a scratch dir");
    let repo = tmp.path();
    let files = vec![
        (
            "2.1.303.jsonl".to_string(),
            format!("{}\n", header("2.1.303")),
        ),
        (
            "codex-0.163.0.jsonl".to_string(),
            "# codex 0.163.0 · lab reduce\n".to_string(),
        ),
    ];
    repo_with(repo, &files);
    let all = committed_in(&CLAUDE, repo).expect("as HEAD holds it");
    assert_eq!(
        all.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
        ["2.1.303.jsonl"],
        "the Claude Code reductions alone"
    );
    assert_eq!(admit("2.1.303", &all).map_err(|r| r.why), Ok(None));
    let steps = repo.join(STEPS);
    std::fs::write(steps.join("codex-0.164.0.jsonl"), "x\n").expect("a codex leftover");
    assert_eq!(
        committed_in(&CLAUDE, repo)
            .expect("a codex file is not Claude Code's")
            .len(),
        1
    );
    std::fs::remove_file(steps.join("codex-0.164.0.jsonl")).expect("removed");
    let leftover = "2.1.399.jsonl";
    std::fs::write(steps.join(leftover), format!("{}\n", header(B))).expect("a leftover");
    let f = committed_in(&CLAUDE, repo).expect_err("an untracked reduction");
    assert_eq!(f.code, 3);
    let msg = f.msg.unwrap_or_default();
    assert!(
        msg.starts_with("NOT RUN: claude reductions")
            && msg.contains(&format!("?? {STEPS}/{leftover}")),
        "{msg}"
    );
    std::fs::remove_file(steps.join(leftover)).expect("removed");
    std::fs::write(steps.join("2.1.303.jsonl"), "changed\n").expect("a change");
    let f = committed_in(&CLAUDE, repo).expect_err("a changed reduction");
    assert!(f.msg.unwrap_or_default().contains("2.1.303.jsonl"));
}

/// The failed checks of a judgment, by what each says.
fn failed(checks: &[Check]) -> Vec<&str> {
    checks
        .iter()
        .filter(|c| !c.ok)
        .map(|c| c.what.as_str())
        .collect()
}

/// A session log of the lab's session as an unmeasured build writes it (the shapes `ctl log` gave
/// for 2.1.316 read partial, in J2-4's lab read): every transcript record strong; call 1 ended by
/// the screen and amended ok by the transcript, call 2 closed ok by the transcript, call 3
/// (declined) amended error and its turn given no end, call 4 (sandboxed, no box) in a turn the
/// transcript opened and closed error; and no wall confirmed.
const PARTIAL: &str = "aterm-ctl: OK 120 v=2 as_of=120
1 open - spawn epoch=1 state=spawning src=aterm conf=exact t=1
20 open r2 agent par=r1 prog=claude agent=claude src=status conf=strong t=2
25 mark r2 source field=transcript state=partial skipped=0 by=aterm src=transcript conf=exact t=3
27 open t1 turn par=r2 by=ctl src=screen conf=strong t=4
28 amend t1 turn vref=v1 by=vendor src=transcript conf=strong t=4
29 amend r2 agent model=claude-opus-5-5 effort=medium ver=2.1.399 by=vendor src=transcript conf=strong t=5
30 close t1 turn out=done dur_ms=40 op_by=ctl src=screen conf=strong t=6
31 amend t1 turn vdur_ms=48 tok_in=1 tok_out=1 by=vendor src=transcript conf=strong t=6
34 open t2 turn par=r2 by=ctl src=screen conf=heuristic t=7
35 open p1 approval par=t2 box=bash:not-read-only src=screen conf=strong t=7
38 open p2 tool par=t2 cls=shell tool=Bash vref=toolu_1 by=vendor src=transcript conf=strong t=7
42 close p1 approval out=passed dur_ms=1 by=ctl src=screen conf=strong t=8
44 close p2 tool out=gone dur_ms=1 src=screen conf=strong t=8
45 close t2 turn out=done dur_ms=1 steps=2 appr=1 tools=shell:1 op_by=ctl src=screen conf=strong t=8
46 amend p2 tool out=ok by=vendor src=transcript conf=strong t=8
47 amend t2 turn vdur_ms=105 tok_in=1 tok_out=1 by=vendor src=transcript conf=strong t=8
56 open t3 turn par=r2 by=ctl src=screen conf=heuristic t=9
57 open p3 approval par=t3 box=bash:not-read-only src=screen conf=strong t=9
59 amend t3 turn vref=v3 by=vendor src=transcript conf=strong t=9
60 open p4 tool par=t3 cls=shell tool=Bash vref=toolu_2 by=vendor src=transcript conf=strong t=9
66 close p3 approval out=passed dur_ms=1 by=ctl src=screen conf=strong t=10
68 close p4 tool out=ok dur_ms=1 dur=wall by=vendor src=transcript conf=strong t=10
69 amend t3 turn vdur_ms=108 tok_in=1 tok_out=1 by=vendor src=transcript conf=strong t=10
71 close t3 turn out=done dur_ms=1 steps=2 appr=1 tools=shell:1 op_by=ctl src=screen conf=strong t=10
77 open t4 turn par=r2 by=ctl src=screen conf=heuristic t=11
78 open p5 approval par=t4 box=bash:not-read-only src=screen conf=strong t=11
80 amend t4 turn vref=v4 by=vendor src=transcript conf=strong t=11
81 open p6 tool par=t4 cls=shell tool=Bash vref=toolu_3 by=vendor src=transcript conf=strong t=11
83 close p5 approval out=passed dur_ms=1 by=ctl src=screen conf=strong t=12
85 close p6 tool out=gone dur_ms=1 src=screen conf=heuristic t=12
86 close t4 turn out=interrupted dur_ms=1 steps=2 appr=1 tools=shell:1 op_by=ctl src=screen conf=strong t=12
87 amend p6 tool out=error by=vendor src=transcript conf=strong t=12
96 open t5 turn par=r2 vref=v5 by=ctl src=transcript conf=strong t=13
97 open p7 tool par=t5 cls=shell tool=Bash vref=toolu_4 by=vendor src=transcript conf=strong t=13
98 close p7 tool out=error dur_ms=60 dur=wall by=vendor src=transcript conf=strong t=13
99 close t5 turn out=done dur_ms=91 steps=1 errs=1 tools=shell:1 op_by=ctl by=vendor src=transcript conf=strong t=13
100 amend t5 turn vdur_ms=105 tok_in=1 tok_out=1 by=vendor src=transcript conf=strong t=13
110 open p8 wall par=r2 wall=usage-session detail=7:27am orphan=1 src=screen conf=strong t=14
111 open t6 turn par=r2 by=ctl src=screen conf=strong t=14
112 amend t6 turn vref=v6 by=vendor src=transcript conf=strong t=14
113 amend t6 turn vdur_ms=19 tok_in=0 tok_out=0 by=vendor src=transcript conf=strong t=14
";

/// The same session as a measured build writes it (the shapes `ctl log` gave for 2.1.312 read
/// on, a call to each of the four answers): every transcript record exact, the transcript ending
/// each Bash call, and the usage wall confirmed.
const ON: &str = "aterm-ctl: OK 120 v=2 as_of=120
1 open - spawn epoch=1 state=spawning src=aterm conf=exact t=1
20 open r2 agent par=r1 prog=claude agent=claude src=status conf=strong t=2
25 mark r2 source field=transcript state=partial skipped=0 by=aterm src=transcript conf=exact t=3
26 mark r2 source field=transcript state=on by=aterm src=transcript conf=exact t=3
27 open t1 turn par=r2 vref=v1 by=ctl src=transcript conf=exact t=4
28 amend r2 agent model=claude-sonnet-5-5 effort=medium ver=2.1.399 by=vendor src=transcript conf=exact t=4
29 close t1 turn out=done dur_ms=40 op_by=ctl by=vendor src=transcript conf=exact t=5
30 amend t1 turn vdur_ms=48 tok_in=1 tok_out=1 by=vendor src=transcript conf=exact t=5
34 open t2 turn par=r2 by=ctl src=screen conf=heuristic t=6
35 open p1 approval par=t2 box=bash:not-read-only src=screen conf=strong t=6
37 amend t2 turn vref=v2 by=vendor src=transcript conf=exact t=6
38 open p2 tool par=t2 cls=shell tool=Bash vref=toolu_1 by=vendor src=transcript conf=exact t=6
42 close p1 approval out=passed dur_ms=1 by=ctl src=screen conf=strong t=7
44 close p2 tool out=ok dur_ms=1 dur=wall by=vendor src=transcript conf=exact t=7
45 close t2 turn out=done dur_ms=1 steps=2 appr=1 tools=shell:1 op_by=ctl by=vendor src=transcript conf=exact t=7
46 amend t2 turn vdur_ms=105 tok_in=1 tok_out=1 by=vendor src=transcript conf=exact t=7
56 open t3 turn par=r2 by=ctl src=screen conf=heuristic t=8
57 open p3 approval par=t3 box=bash:not-read-only src=screen conf=strong t=8
59 amend t3 turn vref=v3 by=vendor src=transcript conf=exact t=8
60 open p4 tool par=t3 cls=shell tool=Bash vref=toolu_2 by=vendor src=transcript conf=exact t=8
66 close p3 approval out=passed dur_ms=1 by=ctl src=screen conf=strong t=9
68 close p4 tool out=ok dur_ms=1 dur=wall by=vendor src=transcript conf=exact t=9
69 close t3 turn out=done dur_ms=1 steps=2 appr=1 tools=shell:1 op_by=ctl by=vendor src=transcript conf=exact t=9
70 amend t3 turn vdur_ms=108 tok_in=1 tok_out=1 by=vendor src=transcript conf=exact t=9
77 open t4 turn par=r2 by=ctl src=screen conf=heuristic t=10
78 open p5 approval par=t4 box=bash:not-read-only src=screen conf=strong t=10
80 amend t4 turn vref=v4 by=vendor src=transcript conf=exact t=10
81 open p6 tool par=t4 cls=shell tool=Bash vref=toolu_3 by=vendor src=transcript conf=exact t=10
83 close p5 approval out=passed dur_ms=1 by=ctl src=screen conf=strong t=11
85 close p6 tool out=error dur_ms=1 dur=wall by=vendor src=transcript conf=exact t=11
86 close t4 turn out=interrupted dur_ms=1 steps=2 appr=1 tools=shell:1 op_by=ctl by=vendor src=transcript conf=exact t=11
96 open t5 turn par=r2 vref=v5 by=ctl src=transcript conf=exact t=12
97 open p7 tool par=t5 cls=shell tool=Bash vref=toolu_4 by=vendor src=transcript conf=exact t=12
98 close p7 tool out=error dur_ms=60 dur=wall by=vendor src=transcript conf=exact t=12
99 close t5 turn out=done dur_ms=91 steps=1 errs=1 tools=shell:1 op_by=ctl by=vendor src=transcript conf=exact t=12
100 amend t5 turn vdur_ms=105 tok_in=1 tok_out=1 by=vendor src=transcript conf=exact t=12
110 open p8 wall par=r2 wall=usage-session detail=7:27am orphan=1 src=screen conf=strong t=13
111 open t6 turn par=r2 vref=v6 by=ctl src=transcript conf=exact t=13
112 close t6 turn out=done dur_ms=18 op_by=ctl by=vendor src=transcript conf=exact t=13
113 amend t6 turn vdur_ms=19 tok_in=0 tok_out=0 by=vendor src=transcript conf=exact t=13
114 amend t6 turn out=wall:usage-session by=vendor src=transcript conf=exact t=14
115 amend p8 wall confirmed=1 by=vendor src=transcript conf=exact t=14
";

/// What the screen showed of the four calls' ends in that session: two ran, the declined one
/// interrupted, and the sandboxed one failed (the lab's own sandbox lets no nested one start).
const SHOWN: [Shown; 4] = [Shown::Ran, Shown::Ran, Shown::Interrupted, Shown::Failed];

#[test]
fn the_session_log_is_judged_as_an_unmeasured_and_a_measured_build_write_it() {
    assert_eq!(
        failed(&judge_log(PARTIAL, B, Expect::Partial, &SHOWN)),
        Vec::<&str>::new()
    );
    assert_eq!(
        failed(&judge_log(ON, B, Expect::On, &SHOWN)),
        Vec::<&str>::new()
    );
    assert_eq!(
        judge_log(ON, B, Expect::On, &SHOWN).len(),
        2 + 2 * CALLS.len() + 4
    );
    let crossed = judge_log(PARTIAL, B, Expect::On, &SHOWN);
    let crossed = failed(&crossed);
    assert!(
        crossed
            .iter()
            .any(|w| w.starts_with("the transcript source reads on"))
            && crossed
                .iter()
                .any(|w| w.starts_with("every transcript record is at conf=exact"))
            && crossed.iter().any(|w| w.contains("confirmed=1")),
        "{crossed:?}"
    );
    let crossed = judge_log(ON, B, Expect::Partial, &SHOWN);
    let crossed = failed(&crossed);
    assert!(
        crossed
            .iter()
            .any(|w| w.starts_with("the transcript source reads partial"))
            && crossed
                .iter()
                .any(|w| w.starts_with("no wall is confirmed")),
        "{crossed:?}"
    );
    for (log, expect, said) in [
        (
            format!("{ON}120 mark - steps dropped=2 src=aterm conf=exact t=15\n"),
            Expect::On,
            "no step is lost (dropped= on mark - steps)",
        ),
        (
            PARTIAL.replace("ver=2.1.399", "ver=2.1.398"),
            Expect::Partial,
            "the transcript names the build",
        ),
        (
            PARTIAL.replace("46 amend p2 tool out=ok", "46 amend p2 tool out=err"),
            Expect::Partial,
            "the transcript ends Bash call 1 ok at conf=strong, as the screen showed it ran",
        ),
        (
            PARTIAL.replace("87 amend p6 tool out=error", "87 amend p6 tool out=ok"),
            Expect::Partial,
            "the transcript ends Bash call 3 error at conf=strong, as the screen showed it interrupted",
        ),
        (
            ON.replace("98 close p7 tool out=error", "98 close p7 tool out=ok"),
            Expect::On,
            "the transcript ends Bash call 4 error at conf=exact, as the screen showed it failed",
        ),
        (
            PARTIAL.replace("113 amend t6 turn vdur_ms=19", "113 amend t6 turn tok=0"),
            Expect::Partial,
            "the transcript ends the 5 turns Claude Code writes an end for (vdur_ms= on 4)",
        ),
        (
            ON.replace("97 open p7 tool", "97 open p7 note"),
            Expect::On,
            "the transcript opens Bash call 4",
        ),
    ] {
        let checks = judge_log(&log, B, expect, &SHOWN);
        let f = failed(&checks);
        assert!(f.iter().any(|w| w.starts_with(said)), "{said}: {f:?}");
    }
    assert_eq!(
        (shell_calls(PARTIAL), shell_calls(ON), shell_calls("")),
        (4, 4, 0)
    );
    assert_eq!((boxes(PARTIAL), boxes(ON)), (3, 3));
    assert!(settled(PARTIAL, Expect::Partial) && settled(ON, Expect::On));
    let unconfirmed = ON.replace("115 amend p8 wall confirmed=1", "115 amend p8 wall");
    assert!(
        !settled(&unconfirmed, Expect::On),
        "the wall not yet confirmed"
    );
    let unended = PARTIAL.replace("100 amend t5 turn vdur_ms=105", "100 amend t5 turn");
    assert!(
        !settled(&unended, Expect::Partial),
        "a turn's end not yet read"
    );
    assert!(!settled("", Expect::Partial));
}

/// The transcript's end of each call is read against the screen: the same session log, judged
/// against a screen that showed the declined call run, fails that call's end and no other.
#[test]
fn each_calls_end_is_read_against_what_the_screen_showed() {
    let ran = [Shown::Ran, Shown::Ran, Shown::Ran, Shown::Failed];
    for (log, expect, conf) in [
        (PARTIAL, Expect::Partial, "strong"),
        (ON, Expect::On, "exact"),
    ] {
        assert_eq!(
            failed(&judge_log(log, B, expect, &ran)),
            [format!(
                "the transcript ends Bash call 3 ok at conf={conf}, as the screen showed it ran"
            )]
        );
    }
    assert_eq!(turns_ended(), 5, "every turn but the declined call's");
}

/// The screen of 2.1.316 after the session's four calls (J2-4's lab read, `aterm ctl text` of a
/// private headless aterm, 144x50; the scratch folder renamed): each call's row, the `⎿` row under
/// it, the stub's replies, and the declined call's notice.
const CALLS_SCREEN: &str = " ▐▛███▛█   Claude Code v2.1.316
▝▜██████▀  Sonnet 5.5 · Claude API
 ▝▝   ▝▝   /private/tmp/x/proj

❯ toolstart quokkafern 1

⏺ Bash(touch /private/tmp/x/proj/quokkafern-1.txt)
  ⎿  Done

⏺ Done 1.

✻ Brewed for 0s · done 10:45 AM

❯ toolstart quokkafern 2

⏺ Bash(touch /private/tmp/x/proj/quokkafern-2.txt)
  ⎿  Done

⏺ Done 2.

✻ Baked for 0s · done 10:46 AM

❯ toolstart quokkafern 3

⏺ Bash(touch /private/tmp/x/proj/quokkafern-3.txt)
  ⎿  Interrupted · What should Claude do instead?

✻ Brewed for 0s · done 10:46 AM

❯ toolstart quokkafern 4

⏺ Bash(touch /private/tmp/x/proj/quokkafern-4.txt)
  ⎿  Error: Exit code 71
     sandbox-exec: sandbox_apply: Operation not permitted

⏺ Done 4.

✻ Worked for 0s · done 10:47 AM

────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
❯
────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
  ⏸ manual mode on · ? for shortcuts
";

/// What the screen shows of each call's end is read off the `⎿` row under its own row: ran,
/// interrupted (the anchor `turn.interrupted`), failed; nothing for a call the screen does not
/// show or one still waiting; and each answer ends a call as only its own way allows.
#[test]
fn a_calls_end_is_read_off_the_row_under_it() {
    let read: Vec<Option<Shown>> = (1..=5).map(|k| shown(CALLS_SCREEN, k)).collect();
    assert_eq!(
        read,
        [
            Some(Shown::Ran),
            Some(Shown::Ran),
            Some(Shown::Interrupted),
            Some(Shown::Failed),
            None
        ]
    );
    let waiting = CALLS_SCREEN.replace("  ⎿  Error: Exit code 71", "  ⎿  Waiting…");
    assert_eq!(shown(&waiting, 4), None, "a call still running has no end");
    let read: Vec<bool> = CALLS.iter().zip(SHOWN).map(|(a, s)| a.ends_as(s)).collect();
    assert_eq!(read, [true; 4]);
    assert!(!Answer::Declined.ends_as(Shown::Ran) && !Answer::Once.ends_as(Shown::Interrupted));
    assert_eq!(
        CALLS.map(Answer::key),
        [Some("1"), Some("2"), Some("escape"), None]
    );
}

/// The check the judge's rule names for a row of run 2's reduction is the one that read the step
/// the fold made of it: a call its opening, a result its end, a turn's end the turns, and a row
/// the fold made no step of (an attachment) what was lost.
#[test]
fn a_rows_check_is_the_one_that_read_the_step_the_fold_made_of_it() {
    let text = std::fs::read_to_string(committed_dir().join("2.1.303.jsonl"))
        .expect("the measured reduction");
    let parsed: Vec<Value> = rows(&text)
        .map(|r| aterm_json::from_str(r).expect("a row"))
        .collect();
    let at = |kind: &str, part: Option<&str>| {
        parsed
            .iter()
            .position(|v| {
                record_kind(v) == kind
                    && part.is_none_or(|p| {
                        v.get("message")
                            .and_then(|m| m.get("content"))
                            .and_then(Value::as_array)
                            .is_some_and(|a| {
                                a.iter()
                                    .any(|x| x.get("type").and_then(Value::as_str) == Some(p))
                            })
                    })
            })
            .expect("a row of that kind")
    };
    let (call, result) = (
        at("assistant/-", Some("tool_use")),
        at("user/-", Some("tool_result")),
    );
    assert_eq!(about_row(&text, call), About::Opens(1));
    assert_eq!(about_row(&text, result), About::Ends(1));
    assert_eq!(
        about_row(&text, at("system/turn_duration", None)),
        About::Turns
    );
    assert_eq!(about_row(&text, at("attachment/-", None)), About::Lost);
    let checks = judge_log(ON, B, Expect::On, &SHOWN);
    assert_eq!(
        check_of_row(&checks, &text, result),
        "result; its check \"the transcript ends Bash call 1 ok at conf=exact, as the screen showed it ran\" passed"
    );
    assert_eq!(
        check_of_row(&checks, &text, at("attachment/-", None)),
        "no step; its check \"no step is lost\" passed"
    );
}

#[test]
fn every_claude_build_in_the_step_testdata_reads_as_measured() {
    let all = committed(&CLAUDE, &committed_dir()).expect("the step testdata");
    assert!(
        all.iter().any(|r| r.build.as_deref() == Some("2.1.303")),
        "2.1.303 is measured"
    );
    for r in &all {
        let b = r
            .build
            .as_deref()
            .unwrap_or_else(|| panic!("{} names its build", r.name));
        assert_eq!(
            admit(b, &all).map_err(|r| r.why),
            Ok(None),
            "{b} reads as measured"
        );
        key_holds(LEG, &r.text, b).unwrap_or_else(|e| panic!("{}: {}", r.name, e.why));
    }
}

#[test]
fn gone_holds_a_build_to_every_transcript_key_the_fold_reads() {
    assert_eq!(
        read_but_not_held(&CLAUDE, &committed_dir()),
        Vec::<String>::new(),
        "keys the fold reads of a committed reduction that TRANSCRIPT_KEYS does not name"
    );
    let old = format!(
        "{}\n{}\n",
        header("2.1.303"),
        r#"{"type":"user","origin":{"kind":"human"},"message":{"role":"user","content":""},"version":"2.1.303"}"#
    );
    let committed = [reduction("2.1.303.jsonl", Some("2.1.303"), &old)];
    let lost = vec![(
        LEG,
        old.replace("2.1.303", B)
            .replace(r#""origin":{"kind":"human"},"#, ""),
    )];
    let r = judge(&CLAUDE, B, &lost, &committed).expect_err("a key the fold reads is gone");
    assert_eq!(r.class, Class::Gone, "{}", r.why);
    assert!(
        r.why.contains("transcript.origin") && r.why.contains("transcript.kind"),
        "{}",
        r.why
    );
}

#[test]
fn the_standing_tests_run_under_the_filters_and_pass_only_when_each_is_seen() {
    for f in [&CLAUDE, &crate::codex_measure::CODEX] {
        for s in f.standing {
            assert!(
                f.filters.iter().any(|p| s.starts_with(p)),
                "{}: {s} runs under one of the filters",
                f.verb
            );
        }
    }
    let ok = ExitStatus::from_raw(0);
    let all: String = STANDING
        .iter()
        .map(|n| format!("test {n} ... ok\n"))
        .collect();
    assert_eq!(
        standing_verdict(&CLAUDE, Ok(()), ok, &all).expect("every one passed"),
        STANDING
    );
    let short = all.replace(&format!("test {} ... ok\n", STANDING[5]), "");
    let f = standing_verdict(&CLAUDE, Ok(()), ok, &short).expect_err("one did not run");
    let msg = f.msg.unwrap_or_default();
    assert!(
        msg.starts_with("REFUSED STANDING:")
            && msg.contains(&format!("did not pass: {}", STANDING[5])),
        "{msg}"
    );
}

#[test]
fn a_transcript_is_read_by_its_kinds_and_names_and_the_note_names_no_home() {
    let v = |t: &str| aterm_json::from_str::<Value>(t).expect("a row");
    assert_eq!(
        record_kind(&v(r#"{"type":"system","subtype":"turn_duration"}"#)),
        "system/turn_duration"
    );
    assert_eq!(record_kind(&v(r#"{"type":"user"}"#)), "user/-");
    for kind in [
        "user/-",
        "assistant/-",
        "system/turn_duration",
        "system/api_retry",
    ] {
        assert!(!inert(kind), "the fold acts on {kind}");
    }
    for kind in ["attachment/-", "tools-used/-", "-/-", "users/-"] {
        assert!(inert(kind), "the fold does not act on {kind}");
    }
    assert_eq!(reduction_name(B, LEG), "2.1.399.jsonl");
    assert!(is_reduction("2.1.303.jsonl"));
    assert!(!is_reduction("codex-0.163.0.jsonl") && !is_reduction("notes.txt"));
    let home = Some("/Users//jsmith");
    assert_eq!(
        said_path("/Users//jsmith/Library/x/claude", home),
        "~/Library/x/claude"
    );
    assert_eq!(
        said_path("/Users//jsmithy/claude", home),
        "/Users//jsmithy/claude"
    );
    assert_eq!(said_path("/opt/claude", home), "/opt/claude");
    assert_eq!(said_path("/opt/claude", None), "/opt/claude");
}

#[test]
fn a_copy_of_the_build_is_the_build_by_its_bytes() {
    let tmp = aterm_tempfile::tempdir().expect("a scratch dir");
    let file = |name: &str, bytes: &[u8]| {
        let p = tmp.path().join(name);
        std::fs::write(&p, bytes).expect("a file");
        p
    };
    let big: Vec<u8> = (0..(3u32 << 20) + 7)
        .map(|i| u8::try_from(i % 251).expect("below 251"))
        .collect();
    let mut other = big.clone();
    *other.last_mut().expect("bytes") ^= 1;
    let (a, b) = (file("a", &big), file("b", &big));
    assert!(same_bytes(&a, &b), "a copy");
    assert!(!same_bytes(&a, &file("c", &other)), "one byte apart");
    assert!(
        !same_bytes(&a, &file("d", &big[..big.len() - 1])),
        "shorter"
    );
    assert!(!same_bytes(&a, &tmp.path().join("none")), "no file");
}
