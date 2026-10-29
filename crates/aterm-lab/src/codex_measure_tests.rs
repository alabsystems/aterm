// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for [`super`]: every refusal class on synthetic inputs (PROOF-RED and NOT-BOUND on a
//! run's checks, STANDING on libtest's text), the step testdata read only as HEAD holds it, the
//! committed builds read as the verb reads them, an interrupt read as the run's, and a refusal,
//! an interrupt, a panic or a second interrupt after the fixtures leaving the checkout as it was.

use std::collections::BTreeSet;
use std::os::unix::process::ExitStatusExt as _;
use std::process::ExitStatus;
use std::time::Duration;

use super::*;
use crate::instance::World;
use crate::measure::{
    Written, aterm_artifact, built, committed, fixed_point, git, header_build, made, passed_tests,
    previous, read_but_not_held, repo_with, standing_verdict, step_shapes,
};
use crate::reduce;
use crate::stop::Stopped;

const COMMITTED: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../aterm-agent/src/harness/testdata/steps"
);

const B: &str = "0.165.0-dev.13586+g133286d36";

fn reduction(name: &str, build: Option<&str>, text: &str) -> Reduction {
    Reduction {
        name: name.to_string(),
        build: build.map(str::to_string),
        text: text.to_string(),
    }
}

fn meta(build: &str) -> String {
    format!(
        r#"{{"timestamp":"2000-01-01T00:00:00.000Z","type":"session_meta","payload":{{"id":"00000000-0000-4000-8000-000000000001","cli_version":"{build}"}}}}"#
    )
}

fn header(build: &str) -> String {
    reduce::header(Program::Codex, Some(build), 0, 1, true)
}

/// The six legs of `build` as reductions, each opening with `build`'s line.
fn six(build: &str) -> Vec<Reduction> {
    let stem = rollout_reduction_stem(build);
    LEGS.iter()
        .map(|leg| {
            reduction(
                &format!("{stem}-{leg}.jsonl"),
                Some(build),
                &format!("{}\n", header(build)),
            )
        })
        .collect()
}

#[test]
fn a_measured_build_is_left_alone_and_a_taken_stem_refuses_stem() {
    let other = "0.165.0-dev.13586+gdeadbeef0";
    let mut committed = six(other);
    committed.push(reduction(
        "codex-alab-0.165.0-dev.13594-first.jsonl",
        Some("0.165.0-dev.13594+g870dabaa6"),
        "",
    ));
    let r = admit(B, &committed).expect_err("the stem is another build's");
    assert_eq!(r.class, Class::Stem, "{}", r.why);
    assert!(r.why.contains(other), "{}", r.why);
    assert_eq!(admit(other, &committed).expect("measured"), None);
    let r = admit("0.165.0-dev.13594+g870dabaa6", &committed).expect_err("one leg of six");
    assert_eq!(r.class, Class::Stem, "{}", r.why);
    assert!(
        r.why.contains("measured only in part")
            && r.why.contains("codex-alab-0.165.0-dev.13594-carry.jsonl"),
        "{}",
        r.why
    );
    let five: Vec<Reduction> = six(other)
        .into_iter()
        .filter(|r| !r.name.ends_with("-usage.jsonl"))
        .collect();
    let r = admit(other, &five).expect_err("five legs of six");
    assert_eq!(r.class, Class::Stem, "{}", r.why);
    assert!(
        r.why
            .ends_with("no codex-alab-0.165.0-dev.13586-usage.jsonl does"),
        "{}",
        r.why
    );
    let vendor = [reduction("codex-0.163.0.jsonl", Some("0.163.0"), "")];
    let r = admit("0.163.0", &vendor).expect_err("a vendor build is never measured here");
    assert_eq!(r.class, Class::Stem, "{}", r.why);
    assert_eq!(
        admit("0.165.0-dev.13600+gabcdef012", &committed).expect("free"),
        Some("codex-alab-0.165.0-dev.13600".to_string())
    );
    let r = admit("0.1.0-dev.5+codex.0.164.0.gabcdef012", &committed).expect_err("no alab stem");
    assert_eq!(r.class, Class::Stem, "{}", r.why);
    let r = admit("0.163.0", &committed).expect_err("a vendor build");
    assert_eq!(r.class, Class::Stem, "{}", r.why);
    assert_eq!(
        Fail::from(r).msg.as_deref().map(|m| &m[..13]),
        Some("REFUSED STEM:")
    );
}

#[test]
fn a_leg_with_no_rollout_or_two_refuses_rollouts() {
    let tmp = aterm_tempfile::tempdir().expect("a scratch dir");
    let home = tmp.path().join("codex-home");
    let r = one_rollout("first", &home).expect_err("none");
    assert_eq!(r.class, Class::Rollouts, "{}", r.why);
    let day = home.join("sessions/2026/10/24");
    std::fs::create_dir_all(&day).expect("a day");
    std::fs::write(day.join("rollout-a.jsonl"), meta(B)).expect("a rollout");
    std::fs::write(day.join("notes.txt"), "not a rollout").expect("a file");
    assert_eq!(
        one_rollout("first", &home).expect("one"),
        day.join("rollout-a.jsonl")
    );
    std::fs::write(day.join("rollout-b.jsonl"), meta(B)).expect("a second rollout");
    let r = one_rollout("first", &home).expect_err("two");
    assert_eq!(r.class, Class::Rollouts, "{}", r.why);
    assert!(r.why.contains("2 rollouts"), "{}", r.why);
}

#[test]
fn a_reduction_whose_own_line_names_another_build_refuses_key() {
    let source = format!("{}\n", meta(B));
    let other = "0.165.0-dev.13586+g133286d37";
    let names = |line: String| {
        move |t: &str| -> Result<String, String> {
            Ok(format!(
                "{line}\n{}\n",
                rows(t).collect::<Vec<_>>().join("\n")
            ))
        }
    };
    let text =
        reduce_leg(&CODEX, "first", &source, B, &names(header(B))).expect("its own line names B");
    assert_eq!(header_build(Program::Codex, &text), Some(B));
    let r =
        reduce_leg(&CODEX, "first", &source, B, &names(header(other))).expect_err("another build");
    assert_eq!(r.class, Class::Key, "{}", r.why);
    assert!(
        r.why.contains("own line names") && r.why.contains(other),
        "{}",
        r.why
    );
    let r = reduce_leg(
        &CODEX,
        "first",
        &source,
        B,
        &names("# codex - · lab reduce".into()),
    )
    .expect_err("no build");
    assert_eq!(r.class, Class::Key, "{}", r.why);
    assert!(r.why.contains("no build"), "{}", r.why);
    let r = reduce_leg(&CODEX, "first", &meta(other), B, &names(header(B)))
        .expect_err("the rollout's own");
    assert_eq!(r.class, Class::Key, "{}", r.why);
    assert!(r.why.contains("row 1"), "{}", r.why);
}

#[test]
fn a_rollout_that_names_another_build_refuses_key() {
    let ok = format!("{}\n{}\n", meta(B), meta(B));
    key_holds("first", &ok, B).expect("every session_meta names the build");
    let r = key_holds(
        "first",
        &format!("{}\n{}\n", meta(B), meta("0.165.0-dev.13586+g133286d37")),
        B,
    )
    .expect_err("a fork names another");
    assert_eq!(r.class, Class::Key, "{}", r.why);
    assert!(r.why.contains("row 2"), "{}", r.why);
    let r = key_holds(
        "first",
        r#"{"type":"event_msg","payload":{"type":"task_complete"}}"#,
        B,
    )
    .expect_err("no session_meta");
    assert_eq!(r.class, Class::Key, "{}", r.why);
    let tmp = aterm_tempfile::tempdir().expect("a scratch dir");
    let r = package_build(tmp.path()).expect_err("no package");
    assert_eq!(r.class, Class::Key, "{}", r.why);
}

#[test]
fn a_reduction_that_is_no_fixed_point_refuses_fixed_point() {
    let grows = |t: &str| -> Result<String, String> { Ok(format!("{t}x\n")) };
    let r = fixed_point("first", "a\n", &grows).expect_err("each pass grows");
    assert_eq!(r.class, Class::FixedPoint, "{}", r.why);
    assert!(r.why.contains("pass 3 is not pass 2"), "{}", r.why);
    let refuses = |_: &str| -> Result<String, String> { Err("planted".into()) };
    let r = fixed_point("first", "a\n", &refuses).expect_err("the reducer refuses");
    assert_eq!(r.class, Class::FixedPoint, "{}", r.why);
    assert!(r.why.contains("pass 1: planted"), "{}", r.why);
    let once = |t: &str| -> Result<String, String> {
        Ok(if t.starts_with('#') {
            t.to_string()
        } else {
            format!("# reduced\n{t}")
        })
    };
    assert_eq!(
        fixed_point("first", "a\n", &once).expect("a fixed point"),
        "# reduced\na\n"
    );
    let path = format!("{COMMITTED}/codex-alab-0.165.0-dev.13594-first.jsonl");
    let committed = std::fs::read_to_string(&path).expect("a committed reduction");
    assert_eq!(
        fixed_point("first", &committed, &reduce_checked(Program::Codex))
            .expect("the real reducer"),
        committed,
        "a committed reduction is its own fixed point"
    );
}

#[test]
fn a_key_or_record_no_build_showed_refuses_new_key() {
    let old = format!(
        "{}\n{}\n{}\n",
        header(B),
        meta(B),
        r#"{"type":"event_msg","payload":{"type":"task_complete","turn_id":"t","duration_ms":3}}"#
    );
    let committed = [reduction(
        "codex-alab-0.165.0-dev.13580-first.jsonl",
        Some("0.165.0-dev.13580+gaaaaaaaaa"),
        &old,
    )];
    let same = vec![("first", old.replace("13580", "13586"))];
    let j = judge(&CODEX, B, &same, &committed).expect("nothing new");
    assert_eq!(
        (j.builds, j.prev.as_deref()),
        (1, Some("0.165.0-dev.13580+gaaaaaaaaa"))
    );
    let key = vec![(
        "first",
        format!(
            "{old}{}\n",
            r#"{"type":"event_msg","payload":{"type":"task_complete","brand_new_key":1}}"#
        ),
    )];
    let r = judge(&CODEX, B, &key, &committed).expect_err("a key no build showed");
    assert_eq!(r.class, Class::NewKey, "{}", r.why);
    assert!(r.why.contains("key \"brand_new_key\""), "{}", r.why);
    let kind = vec![(
        "first",
        format!(
            "{old}{}\n",
            r#"{"type":"event_msg","payload":{"type":"brand_new_event"}}"#
        ),
    )];
    let r = judge(&CODEX, B, &kind, &committed).expect_err("a record no build showed");
    assert_eq!(r.class, Class::NewKey, "{}", r.why);
    assert!(
        r.why.contains("record event_msg/brand_new_event"),
        "{}",
        r.why
    );
    // A name the reducer gives a key (`k<n>`) holds nothing of the program's: a build whose
    // reduction renames more keys than any measured one showed shows no new key by it.
    let renamed = vec![(
        "first",
        format!(
            "{old}{}\n",
            r#"{"type":"event_msg","payload":{"type":"task_complete","k9":{"k10":1}}}"#
        ),
    )];
    judge(&CODEX, B, &renamed, &committed).expect("the reducer's key names are no new key");
}

#[test]
fn a_rollout_key_the_previous_build_showed_refuses_gone() {
    let prev = "0.165.0-dev.13580+gaaaaaaaaa";
    let old = format!(
        "{}\n{}\n{}\n",
        header(prev),
        meta(prev),
        r#"{"type":"event_msg","payload":{"type":"task_complete","turn_id":"t","duration_ms":3,"extra":1}}"#
    );
    let committed = [reduction(
        "codex-alab-0.165.0-dev.13580-first.jsonl",
        Some(prev),
        &old,
    )];
    let lost = vec![("first", old.replace(",\"duration_ms\":3", ""))];
    let r = judge(&CODEX, B, &lost, &committed).expect_err("a key the fold reads is gone");
    assert_eq!(r.class, Class::Gone, "{}", r.why);
    assert!(r.why.contains("rollout.duration_ms"), "{}", r.why);
    let unread = vec![("first", old.replace(",\"extra\":1", ""))];
    let j = judge(&CODEX, B, &unread, &committed).expect("a key no reader reads may go");
    assert_eq!(
        j.noted,
        ["first: key \"extra\" gone since 0.165.0-dev.13580+gaaaaaaaaa"]
    );
    let other_leg = vec![("carry", old.replace(",\"duration_ms\":3", ""))];
    judge(&CODEX, B, &other_leg, &committed).expect("only the same leg is held to it");
}

#[test]
fn gone_holds_a_build_to_every_rollout_key_the_fold_reads() {
    assert_eq!(
        read_but_not_held(&CODEX, Path::new(COMMITTED)),
        Vec::<String>::new(),
        "keys the fold reads of a committed reduction that ROLLOUT_KEYS does not name"
    );
}

/// The codex-alab builds measured before 13594, which its measurement was held to.
const BEFORE_13594: [&str; 4] = [
    "0.164.0-dev.12479+g07a25d86a",
    "0.164.0-dev.12521+g3d3eb12d8",
    "0.164.0-dev.12672+g089291d37",
    "0.164.0-dev.13172+g32a73bca2",
];

#[test]
fn every_codex_alab_build_in_the_step_testdata_is_its_six_legs() {
    let all = committed(&CODEX, Path::new(COMMITTED)).expect("the step testdata");
    let builds: BTreeSet<&str> = all
        .iter()
        .filter_map(|r| r.build.as_deref())
        .filter(|b| is_alab(b))
        .collect();
    for b in BEFORE_13594.iter().chain(&["0.165.0-dev.13594+g870dabaa6"]) {
        assert!(builds.contains(b), "{b} is measured");
    }
    for b in builds {
        assert_eq!(
            admit(b, &all).map_err(|r| r.why),
            Ok(None),
            "{b} reads as measured, its six legs whole"
        );
    }
}

#[test]
fn the_committed_builds_read_as_measured_with_nothing_the_verb_would_refuse() {
    let all = committed(&CODEX, Path::new(COMMITTED)).expect("the step testdata");
    let build = "0.165.0-dev.13594+g870dabaa6";
    assert_eq!(admit(build, &all).expect("listed"), None);
    // 13594 as the verb would have judged it: against the builds measured before it, whatever
    // is measured later.
    let others: Vec<Reduction> = all
        .iter()
        .filter(|r| {
            r.build
                .as_deref()
                .is_some_and(|b| !is_alab(b) || BEFORE_13594.contains(&b))
        })
        .map(|r| reduction(&r.name, r.build.as_deref(), &r.text))
        .collect();
    assert_eq!(
        admit(build, &others).expect("unlisted once its reductions are taken away"),
        Some("codex-alab-0.165.0-dev.13594".to_string())
    );
    let legs: Vec<(&str, String)> = LEGS
        .iter()
        .map(|leg| {
            let name = format!("codex-alab-0.165.0-dev.13594-{leg}.jsonl");
            let r = all
                .iter()
                .find(|r| r.name == name)
                .expect("a committed leg");
            (*leg, r.text.clone())
        })
        .collect();
    let j = judge(&CODEX, build, &legs, &others).expect("13594 as the verb would have judged it");
    assert_eq!(j.prev.as_deref(), Some("0.164.0-dev.13172+g32a73bca2"));
    assert_eq!(j.builds, 4);
    assert!(j.noted.is_empty(), "{:?}", j.noted);
    assert_eq!(
        step_shapes(Program::Codex, &legs[2].1),
        [
            "prompt",
            "model",
            "call exec_command",
            "result",
            "end",
            "prompt",
            "end",
            "prompt",
            "end"
        ],
        "the approval leg"
    );
    assert_eq!(
        previous(&CODEX, "0.164.0-dev.12672+g089291d37", &others).as_deref(),
        Some("0.164.0-dev.12521+g3d3eb12d8")
    );
    assert_eq!(
        previous(&CODEX, "0.164.0-dev.12000+gaaaaaaaaa", &others).as_deref(),
        Some("0.164.0-dev.13172+g32a73bca2"),
        "none older: the newest"
    );
    assert!(order_of("0.165.0-dev.9+gaaaaaaaaa") < order_of("0.165.0-dev.10+gaaaaaaaaa"));
    assert!(order_of("0.164.0-dev.99999+gaaaaaaaaa") < order_of("0.165.0-dev.1+gaaaaaaaaa"));
}

fn status(repo: &Path) -> String {
    git(repo, &["status", "--porcelain", "--untracked-files=all"]).expect("git status")
}

#[test]
fn a_refusal_after_the_fixtures_leaves_the_checkout_as_it_was() {
    let tmp = aterm_tempfile::tempdir().expect("a scratch dir");
    let repo = tmp.path();
    git(repo, &["init", "-q"]).expect("git init");
    let steps = repo.join("steps");
    std::fs::create_dir(&steps).expect("the steps dir");
    std::fs::write(steps.join("codex-0.163.0.jsonl"), "kept\n").expect("a file before");
    let before = status(repo);
    let fixtures: Vec<(PathBuf, String)> = LEGS
        .iter()
        .map(|leg| {
            (
                steps.join(format!("codex-alab-0.165.0-dev.13586-{leg}.jsonl")),
                (*leg).to_string(),
            )
        })
        .collect();
    let mut seen = 0;
    let f = landed(&CODEX, &fixtures, |w| -> Result<(), Fail> {
        seen = status(repo).lines().count();
        w.create(&repo.join("note.md"), b"a note")
            .map_err(Fail::die)?;
        Err(Refusal::new(Class::Standing, "planted").into())
    })
    .expect_err("the planted refusal");
    let msg = f.msg.unwrap_or_default();
    assert!(msg.starts_with("REFUSED STANDING: planted"), "{msg}");
    assert!(msg.contains("removed the 7 files it wrote"), "{msg}");
    assert_eq!(seen, 7, "the six fixtures and the file before were there");
    assert_eq!(status(repo), before, "the checkout is as it was");
    assert_eq!(
        std::fs::read_to_string(steps.join("codex-0.163.0.jsonl")).expect("kept"),
        "kept\n"
    );
    let taken = vec![(steps.join("codex-0.163.0.jsonl"), "over".to_string())];
    let f = landed(&CODEX, &taken, |_| Ok(())).expect_err("never written over");
    assert!(f.msg.unwrap_or_default().contains("never written over"));
    assert_eq!(status(repo), before);
    let f = landed(&CODEX, &fixtures, |_| -> Result<(), Fail> {
        Err(Stopped(libc::SIGINT).into())
    })
    .expect_err("the run's interrupt");
    assert_eq!(
        (f.stopped, f.code),
        (Some(libc::SIGINT), 128 + libc::SIGINT)
    );
    let msg = f.msg.unwrap_or_default();
    assert!(
        msg.starts_with("interrupted by signal 2; removed the 6 files"),
        "{msg}"
    );
    assert_eq!(status(repo), before, "an interrupt removes them too");
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        landed(&CODEX, &fixtures, |_| -> Result<(), Fail> {
            panic!("planted")
        })
    }));
    assert!(panicked.is_err(), "the planted panic");
    assert_eq!(status(repo), before, "a panic removes them by the drop");
    let ours = |p: &PathBuf| p.starts_with(repo);
    assert!(
        !made().iter().any(ours),
        "nothing of ours is left for a hard exit"
    );
    let (w, ()) = landed(&CODEX, &fixtures, |_| Ok(())).expect("written");
    assert_eq!(w.len(), 6);
    assert_eq!(status(repo).lines().count(), 7);
    assert!(!made().iter().any(ours), "kept: no hard exit removes them");
}

const PROBE: &str = "ATERM_LAB_CODEX_MEASURE_PROBE";

#[test]
fn a_second_interrupt_removes_what_the_run_wrote_before_it_ends_the_process() {
    const TEST: &str = "codex_measure::tests::a_second_interrupt_removes_what_the_run_wrote_before_it_ends_the_process";
    if let Some(dir) = std::env::var_os(PROBE) {
        crate::sys::start_signal(libc::SIGINT, false);
        crate::stop::install();
        crate::stop::at_hard_exit(undo_on_hard_exit);
        let dir = PathBuf::from(dir);
        let mut w = Written::default();
        for leg in LEGS {
            w.create(&dir.join(format!("{leg}.jsonl")), b"x\n")
                .expect("a fixture");
        }
        println!(
            "PROBE made {}",
            std::fs::read_dir(&dir).expect("the dir").count()
        );
        let me = i32::try_from(std::process::id()).expect("pid");
        crate::sys::kill(me, libc::SIGINT).expect("the first interrupt");
        assert_eq!(
            crate::stop::sleep(Duration::from_mins(1)),
            Err(Stopped(libc::SIGINT)),
            "the first interrupt is noted"
        );
        crate::sys::kill(me, libc::SIGINT).expect("the second interrupt");
        std::thread::sleep(Duration::from_mins(1));
        println!("PROBE still running");
        std::mem::forget(w);
        std::process::exit(0);
    }
    let tmp = aterm_tempfile::tempdir().expect("a scratch dir");
    let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"));
    cmd.args(["--exact", TEST, "--nocapture", "--test-threads=1"])
        .env(PROBE, tmp.path());
    let out = aterm_uds::deadline::output_until(
        &mut cmd,
        std::time::Instant::now() + Duration::from_mins(2),
    )
    .expect("the probe ends within the hang detector");
    let said = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(128 + libc::SIGINT), "{said}");
    assert!(said.contains("PROBE made 6"), "{said}");
    let left: Vec<_> = std::fs::read_dir(tmp.path())
        .expect("the dir")
        .filter_map(Result::ok)
        .map(|e| e.file_name())
        .collect();
    assert!(left.is_empty(), "the hard exit removed them: {left:?}");
}

#[test]
fn the_step_testdata_is_read_only_as_head_holds_it() {
    let tmp = aterm_tempfile::tempdir().expect("a scratch dir");
    let repo = tmp.path();
    let measured = "0.165.0-dev.13580+gaaaaaaaaa";
    let files: Vec<(String, String)> = six(measured)
        .into_iter()
        .map(|r| (r.name, r.text))
        .chain([("claude-2.1.303.jsonl".to_string(), "{}\n".to_string())])
        .collect();
    repo_with(repo, &files);
    let all = committed_in(&CODEX, repo).expect("as HEAD holds it");
    assert_eq!(all.len(), 6, "the codex reductions alone");
    assert_eq!(admit(measured, &all).expect("measured"), None);
    let steps = repo.join(STEPS);
    let leftover = "codex-alab-0.165.0-dev.13586-first.jsonl";
    std::fs::write(steps.join(leftover), format!("{}\n", header(B))).expect("a leftover");
    let f = committed_in(&CODEX, repo).expect_err("an untracked reduction");
    assert_eq!(f.code, 3);
    let msg = f.msg.unwrap_or_default();
    assert!(
        msg.starts_with("NOT RUN: codex reductions")
            && msg.contains(&format!("?? {STEPS}/{leftover}")),
        "{msg}"
    );
    assert_eq!(
        admit(B, &committed(&CODEX, &steps).expect("the files")).map_err(|r| r.class),
        Err(Class::Stem),
        "and were it read from the files, one leg would not read as measured either"
    );
    std::fs::remove_file(steps.join(leftover)).expect("removed");
    let changed = steps.join(&files[0].0);
    std::fs::write(&changed, "changed\n").expect("a change");
    let f = committed_in(&CODEX, repo).expect_err("a changed reduction");
    assert!(f.msg.unwrap_or_default().contains(&files[0].0));
    git(repo, &["checkout", "-q", "--", STEPS]).expect("restored");
    std::fs::write(repo.join(".gitignore"), "*.ignored.jsonl\n").expect("an ignore");
    std::fs::write(steps.join("codex-alab-x.ignored.jsonl"), "x\n").expect("ignored");
    let f = committed_in(&CODEX, repo).expect_err("an ignored reduction");
    assert!(
        f.msg.unwrap_or_default().contains("!! "),
        "ignored files are named"
    );
    std::fs::remove_file(steps.join("codex-alab-x.ignored.jsonl")).expect("removed");
    std::fs::write(steps.join("notes.txt"), "x\n").expect("no reduction");
    assert_eq!(
        committed_in(&CODEX, repo)
            .expect("only codex reductions are held")
            .len(),
        6
    );
}

#[test]
fn a_proof_run_with_a_fail_refuses_its_class_and_one_without_passes() {
    let run = |fails: &[&str]| Run {
        world: World::new("").expect("a world"),
        oks: 3,
        fails: fails.iter().map(|f| (*f).to_string()).collect(),
        fences: Vec::new(),
        took: None,
    };
    let ok = run(&[]);
    gate(Class::ProofRed, &ok).expect("no FAIL");
    gate(Class::NotBound, &ok).expect("no FAIL");
    let failed = run(&["rollout: past the tap's fence"]);
    for (class, word) in [
        (Class::ProofRed, "REFUSED PROOF-RED: 1 check(s) failed"),
        (Class::NotBound, "REFUSED NOT-BOUND: 1 check(s) failed"),
    ] {
        let f = gate(class, &failed).expect_err("a FAIL");
        assert_eq!(f.code, 1);
        let msg = f.msg.unwrap_or_default();
        assert!(
            msg.starts_with(word)
                && msg.contains("rollout: past the tap's fence")
                && msg.ends_with(&format!(
                    "its world is kept at {}",
                    failed.world.dir.display()
                )),
            "{msg}"
        );
    }
}

#[test]
fn the_standing_tests_pass_only_when_each_is_seen_passing_and_an_interrupt_is_the_runs() {
    let ok = ExitStatus::from_raw(0);
    let failed = ExitStatus::from_raw(101 << 8);
    let all: String = STANDING
        .iter()
        .map(|n| format!("test {n} ... ok\n"))
        .collect();
    assert_eq!(
        standing_verdict(&CODEX, Ok(()), ok, &all).expect("every one passed"),
        STANDING
    );
    let short = all.replace(&format!("test {} ... ok\n", STANDING[2]), "");
    let f = standing_verdict(&CODEX, Ok(()), ok, &short).expect_err("one did not run");
    let msg = f.msg.unwrap_or_default();
    assert!(
        msg.starts_with("REFUSED STANDING:")
            && msg.contains(&format!("did not pass: {}", STANDING[2])),
        "{msg}"
    );
    let red = format!("{all}test x::y ... FAILED\n");
    let f = standing_verdict(&CODEX, Ok(()), failed, &red).expect_err("the run failed");
    let msg = f.msg.unwrap_or_default();
    assert!(
        msg.starts_with("REFUSED STANDING:") && msg.contains("(failed: x::y; did not pass: none)"),
        "{msg}"
    );
    let f = standing_verdict(
        &CODEX,
        Err(Stopped(libc::SIGINT)),
        ExitStatus::from_raw(libc::SIGINT),
        "",
    )
    .expect_err("interrupted");
    assert_eq!((f.stopped, f.code, f.msg), (Some(libc::SIGINT), 130, None));
}

#[test]
fn the_build_and_test_output_are_read_as_cargo_and_libtest_write_them() {
    let lines = [
        r#"{"reason":"compiler-artifact","target":{"name":"aterm_agent","kind":["lib"]},"executable":null}"#,
        r#"{"reason":"compiler-artifact","target":{"name":"aterm","kind":["bin"]},"executable":"/t/debug/aterm"}"#,
        r#"{"reason":"build-finished","success":true}"#,
    ]
    .join("\n");
    assert_eq!(
        aterm_artifact(&lines),
        Some(PathBuf::from("/t/debug/aterm"))
    );
    assert_eq!(aterm_artifact(lines.lines().next().expect("a row")), None);
    let ok = ExitStatus::from_raw(0);
    assert_eq!(
        built(Ok(()), ok, &lines).expect("built"),
        PathBuf::from("/t/debug/aterm")
    );
    let f = built(Ok(()), ExitStatus::from_raw(101 << 8), &lines).expect_err("a failed build");
    assert_eq!(f.code, 3);
    assert!(
        f.msg
            .unwrap_or_default()
            .starts_with("NOT RUN: `targo --unverified build -p aterm` exited")
    );
    let f = built(Ok(()), ok, "").expect_err("no aterm named");
    assert_eq!(
        f.msg.as_deref(),
        Some("NOT RUN: the build named no aterm binary")
    );
    let f = built(
        Err(Stopped(libc::SIGINT)),
        ExitStatus::from_raw(libc::SIGINT),
        "",
    )
    .expect_err("interrupted");
    assert_eq!((f.stopped, f.code, f.msg), (Some(libc::SIGINT), 130, None));
    let out = "running 3 tests\ntest anchors::tests::a ... ok\ntest anchors::tests::b ... FAILED\ntest x ... ignored\n";
    assert_eq!(
        passed_tests(out).into_iter().collect::<Vec<_>>(),
        ["anchors::tests::a"]
    );
}
