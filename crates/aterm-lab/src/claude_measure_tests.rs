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

const COMMITTED: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../aterm-agent/src/harness/testdata/steps"
);

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

#[test]
fn a_key_or_record_no_claude_build_showed_refuses_new_key() {
    let old = format!(
        "{}\n{}\n",
        header("2.1.303"),
        r#"{"type":"system","subtype":"turn_duration","durationMs":3,"isMeta":false,"version":"2.1.303"}"#
    );
    let committed = [reduction("2.1.303.jsonl", Some("2.1.303"), &old)];
    let same = vec![(LEG, old.replace("2.1.303", B))];
    let j = judge(&CLAUDE, B, &same, &committed).expect("nothing new");
    assert_eq!((j.builds, j.prev.as_deref()), (1, Some("2.1.303")));
    let key = vec![(
        LEG,
        format!("{}{}\n", same[0].1, r#"{"type":"user","brand_new_key":1}"#),
    )];
    let r = judge(&CLAUDE, B, &key, &committed).expect_err("a key no build showed");
    assert_eq!(r.class, Class::NewKey, "{}", r.why);
    assert!(r.why.contains("key \"brand_new_key\""), "{}", r.why);
    let kind = vec![(
        LEG,
        format!("{}{}\n", same[0].1, r#"{"type":"tools-used"}"#),
    )];
    let r = judge(&CLAUDE, B, &kind, &committed).expect_err("a record no build showed");
    assert_eq!(r.class, Class::NewKey, "{}", r.why);
    assert!(r.why.contains("record tools-used/-"), "{}", r.why);
    let renamed = vec![(
        LEG,
        format!(
            "{}{}\n",
            same[0].1, r#"{"type":"system","subtype":"turn_duration","k9":{"k10":1}}"#
        ),
    )];
    judge(&CLAUDE, B, &renamed, &committed).expect("the reducer's key names are no new key");
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
fn failed(checks: &[(bool, String)]) -> Vec<&str> {
    checks
        .iter()
        .filter(|(ok, _)| !ok)
        .map(|(_, what)| what.as_str())
        .collect()
}

/// A session log of the lab's session as an unmeasured build writes it (the shapes `ctl log`
/// gave for 2.1.312 read partial): every transcript record strong, the screen ending the Bash
/// call and the transcript amending it ok, and no wall confirmed.
const PARTIAL: &str = "aterm-ctl: OK 30 v=2 as_of=30
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
50 open p3 wall par=r2 wall=usage-session detail=7:27am orphan=1 src=screen conf=strong t=9
51 open t3 turn par=r2 by=ctl src=screen conf=strong t=9
52 amend t3 turn vref=v3 by=vendor src=transcript conf=strong t=9
53 amend t3 turn vdur_ms=19 tok_in=0 tok_out=0 by=vendor src=transcript conf=strong t=9
";

/// The same session as a measured build writes it (the shapes `ctl log` gave for 2.1.312 read
/// on): every transcript record exact, the transcript ending the Bash call, and the usage wall
/// confirmed.
const ON: &str = "aterm-ctl: OK 56 v=2 as_of=56
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
50 open p3 wall par=r2 wall=usage-session detail=7:27am orphan=1 src=screen conf=strong t=8
51 open t3 turn par=r2 vref=v3 by=ctl src=transcript conf=exact t=8
52 close t3 turn out=done dur_ms=18 op_by=ctl by=vendor src=transcript conf=exact t=8
53 amend t3 turn vdur_ms=19 tok_in=0 tok_out=0 by=vendor src=transcript conf=exact t=8
54 amend t3 turn out=wall:usage-session by=vendor src=transcript conf=exact t=9
55 amend p3 wall confirmed=1 by=vendor src=transcript conf=exact t=9
";

#[test]
fn the_session_log_is_judged_as_an_unmeasured_and_a_measured_build_write_it() {
    assert_eq!(
        failed(&judge_log(PARTIAL, B, Expect::Partial)),
        Vec::<&str>::new()
    );
    assert_eq!(failed(&judge_log(ON, B, Expect::On)), Vec::<&str>::new());
    assert_eq!(judge_log(ON, B, Expect::On).len(), 8);
    let crossed = judge_log(PARTIAL, B, Expect::On);
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
    let crossed = judge_log(ON, B, Expect::Partial);
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
            format!("{ON}60 mark - steps dropped=2 src=aterm conf=exact t=10\n"),
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
            "the transcript ends the Bash call ok",
        ),
        (
            PARTIAL.replace("53 amend t3 turn vdur_ms=19", "53 amend t3 turn tok=0"),
            Expect::Partial,
            "the transcript ends the three turns (vdur_ms= on 2)",
        ),
        (
            ON.replace("38 open p2 tool", "38 open p2 note"),
            Expect::On,
            "the transcript opens the Bash call",
        ),
    ] {
        let checks = judge_log(&log, B, expect);
        let f = failed(&checks);
        assert!(f.iter().any(|w| w.starts_with(said)), "{said}: {f:?}");
    }
    assert!(shell_opened(PARTIAL) && shell_opened(ON) && !shell_opened(""));
    assert!(settled(PARTIAL, Expect::Partial) && settled(ON, Expect::On));
    let unconfirmed = ON.replace("55 amend p3 wall confirmed=1", "55 amend p3 wall");
    assert!(
        !settled(&unconfirmed, Expect::On),
        "the wall not yet confirmed"
    );
    assert!(!settled("", Expect::Partial));
}

#[test]
fn every_claude_build_in_the_step_testdata_reads_as_measured() {
    let all = committed(&CLAUDE, Path::new(COMMITTED)).expect("the step testdata");
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
        read_but_not_held(&CLAUDE, Path::new(COMMITTED)),
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
