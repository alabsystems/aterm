// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const TREE: &str = "1111111111111111111111111111111111111111";
const COMMIT: &str = "2222222222222222222222222222222222222222";
const RUN: &str = "20261105T120000-1";

fn u(n: u8) -> String {
    format!("{n:02x}").repeat(32)
}

#[test]
fn later_cuts_offer_ancestor_lint_facts_without_reviving_newer_tombstones() {
    let f = Fixture::new();
    let repository = f._dir.path().join("repository");
    std::fs::create_dir_all(&repository).unwrap();
    let git = crate::ledger::GitCli::new(&repository);
    let call = |args: &[&str]| {
        let output = git.git(args).unwrap();
        assert!(output.success(), "{}", output.stderr_utf8());
        output.stdout_utf8().trim().to_owned()
    };
    call(&["init", "--quiet", "-b", "main"]);
    call(&["config", "user.name", "Qualification fixture"]);
    call(&["config", "user.email", "fixture@example.invalid"]);
    let commit = |text: &str| {
        std::fs::write(repository.join("source"), text).unwrap();
        call(&["add", "source"]);
        call(&["commit", "--quiet", "-m", text]);
        call(&["rev-parse", "HEAD^{tree}"])
    };
    commit("base");
    call(&["checkout", "--quiet", "-b", "cut"]);
    let old = commit("old qualified cut");
    let newer = commit("cut with a failed package");
    call(&["checkout", "--quiet", "main"]);
    commit("new main source");
    call(&[
        "merge",
        "--quiet",
        "--no-ff",
        "-s",
        "ours",
        "cut",
        "-m",
        "record cut without changing main",
    ]);
    call(&["commit", "--quiet", "--allow-empty", "-m", "next cut"]);
    let head = call(&["rev-parse", "HEAD"]);
    let tree = call(&["rev-parse", "HEAD^{tree}"]);
    let seed = |tree: &str, packages: Vec<LintPackage>| {
        let mut facts = f.facts("prior-seal", "unused");
        facts.tree = tree;
        let mut row = record(&facts, (0, &u(9)), "lint", OK, None);
        row.lint_packages = packages;
        let at = path(&f.root, tree, "prior-seal", "lint", None);
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        write(&at, &row).unwrap();
    };
    seed(
        &old,
        vec![
            lint_package("stable", 1, OK),
            lint_package("changed", 2, OK),
        ],
    );
    seed(&newer, vec![lint_package("changed", 3, FAIL)]);
    let candidates = ancestor_lint_candidates(&git, &f.root, &head, &tree).unwrap();
    assert_eq!(candidates, [lint_package("stable", 1, OK)]);
    assert!(
        !has_records(&f.root, &tree).unwrap(),
        "offering facts writes no current verdict"
    );
}

/// A record as large as a whole-tree check's (over the old 1 MiB cap) is written and read
/// back; one over the bound its reader holds is never written.
///
/// NEGATIVE CONTROL: with the old 1 MiB `MAX_RECORD_BYTES` the read fails with "file exceeds
/// bound", the refusal of 0.193.0's step 1 on a 1.3 MB `fmt` record.
#[test]
fn a_whole_tree_record_reads_back_and_an_unreadable_one_is_never_written() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let at = dir.path().join("fmt.toml");
    let facts_log = dir.path().join("run.log");
    let facts = RunFacts {
        commit: COMMIT,
        tree: TREE,
        seal: "seal",
        executable: None,
        log: &facts_log,
        run: RUN,
        asked: &[],
        content: None,
    };
    let mut big = record(&facts, (0, "sha"), "fmt", "ok", None);
    big.rows = (0..40_000)
        .map(|n| format!("ok crates/member-{n:05}/src/lib.rs {}", u(7)))
        .collect();
    write(&at, &big).unwrap();
    assert!(std::fs::metadata(&at).unwrap().len() > 1024 * 1024);
    match read(&at).unwrap() {
        Read::Record(back) => assert_eq!(back.rows.len(), 40_000),
        _ => panic!("the record it wrote did not read back"),
    }
    let small = dir.path().join("over.toml");
    let refused = write_within(&small, &big, 1024).unwrap_err();
    assert!(refused.contains("over the 1024-byte bound"), "{refused}");
    assert!(!small.exists());
}

fn finished(check: &str, outcome: &str) -> aterm_json::Value {
    aterm_json::json!({"schema":1,"event":"check-finished","check":check,"outcome":outcome,
        "seconds":2.0,"detail":"d","rows":[{"outcome":outcome,"detail":"row"}]})
}

fn bin(spec: &str, sha: u8, outcome: &str) -> Binary {
    Binary {
        spec: spec.into(),
        sha256: u(sha),
        outcome: outcome.into(),
        content_key: Some(u(sha.wrapping_add(32))),
        inputs_bounded: true,
        input_reason: None,
        json: Some(false),
        execution_s: f64::from(sha),
    }
}

fn lint_package(spec: &str, key: u8, outcome: &str) -> LintPackage {
    LintPackage {
        spec: spec.into(),
        outcome: outcome.into(),
        content_key: Some(u(key)),
        inputs_bounded: true,
        input_reason: None,
        execution_s: f64::from(key),
        reused: false,
    }
}

fn appkit_redraw_group(key: u8, executable: u8) -> aterm_verify::appkit_content::GroupProof {
    aterm_verify::appkit_content::GroupProof {
        schema: aterm_verify::appkit_content::SCHEMA,
        group: "redraw-conformance".into(),
        content_key: u(key),
        executables: vec![aterm_verify::appkit_content::Executable {
            name: "aterm-redraw-conformance".into(),
            sha256: u(executable),
        }],
    }
}

fn names(failed: &[Failed]) -> Vec<&str> {
    failed.iter().map(|f| f.check.as_str()).collect()
}

struct Fixture {
    _dir: aterm_tempfile::TempDir,
    root: PathBuf,
    log: PathBuf,
    asked: Vec<String>,
}

impl Fixture {
    fn new() -> Self {
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path().join("verified");
        let log = dir.path().join("06-verify.log");
        std::fs::write(&log, "log\n").unwrap();
        Self {
            _dir: dir,
            root,
            log,
            asked: release_names(),
        }
    }

    fn facts<'a>(&'a self, seal: &'a str, executable: &'a str) -> RunFacts<'a> {
        RunFacts {
            commit: COMMIT,
            tree: TREE,
            seal,
            executable: Some(executable),
            log: &self.log,
            run: RUN,
            asked: &self.asked,
            content: None,
        }
    }

    /// The run's end, as `write_run` takes it.
    fn end(
        &self,
        seal: &str,
        executable: &str,
        exit: i32,
        events: Result<&RunEvents, &str>,
    ) -> Result<Vec<String>, String> {
        write_run(
            &self.root,
            &self.facts(seal, executable),
            RunEnd {
                exit,
                log_sha256: &u(9),
                events,
            },
        )
    }

    fn begin(&self, seal: &str, executable: &str) -> Result<PathBuf, String> {
        begin_run(&self.root, &self.facts(seal, executable))
    }

    /// One whole run: begun, then ended.
    fn write(&self, seal: &str, executable: &str, exit: i32, events: &RunEvents) -> Vec<String> {
        self.begin(seal, executable).unwrap();
        self.end(seal, executable, exit, Ok(events)).unwrap()
    }

    fn judge(&self, seal: &str, executable: Option<&str>) -> Checks {
        checks_in(&self.root, COMMIT, TREE, executable, || Ok(seal.to_owned())).unwrap()
    }
}

fn all_ok() -> RunEvents {
    RunEvents {
        finished: release_names().iter().map(|c| finished(c, OK)).collect(),
        ..RunEvents::default()
    }
}

#[test]
fn source_preflight_success_cannot_satisfy_qualification_lint() {
    let f = Fixture::new();
    let asked: Vec<_> = aterm_verify::release_input::CHECKS
        .into_iter()
        .map(|check| check.name().to_owned())
        .collect();
    let mut facts = f.facts("source-only", "");
    facts.executable = None;
    facts.asked = &asked;
    begin_run(&f.root, &facts).unwrap();
    let events = RunEvents {
        finished: asked.iter().map(|name| finished(name, OK)).collect(),
        ..RunEvents::default()
    };
    write_run(
        &f.root,
        &facts,
        RunEnd {
            exit: 0,
            log_sha256: "",
            events: Ok(&events),
        },
    )
    .unwrap();
    assert!(input_missing(&f.root, TREE, facts.seal).unwrap().is_empty());
    let qualification = f.judge(facts.seal, None);
    assert!(
        qualification
            .run(Half::Tree)
            .checks
            .iter()
            .any(|check| check == "lint"),
        "{}",
        qualification.line()
    );
    assert!(!qualification.complete());
}

#[test]
fn input_records_resume_only_missing_and_never_reroll_red() {
    let f = Fixture::new();
    let asked: Vec<_> = aterm_verify::release_input::CHECKS
        .into_iter()
        .map(|check| check.name().to_owned())
        .collect();
    let mut facts = f.facts("input-cone-seal", "");
    facts.executable = None;
    facts.asked = &asked;
    assert_eq!(input_missing(&f.root, TREE, facts.seal).unwrap(), asked);
    begin_run(&f.root, &facts).unwrap();
    assert!(input_missing(&f.root, TREE, facts.seal).is_err());
    let events = RunEvents {
        finished: asked.iter().map(|name| finished(name, OK)).collect(),
        ..RunEvents::default()
    };
    write_run(
        &f.root,
        &facts,
        RunEnd {
            exit: 0,
            log_sha256: "",
            events: Ok(&events),
        },
    )
    .unwrap();
    assert!(input_missing(&f.root, TREE, facts.seal).unwrap().is_empty());
    assert_eq!(
        input_missing(&f.root, TREE, "changed-cone-seal").unwrap(),
        asked
    );
    assert_eq!(
        input_missing(&f.root, "changed-tree", facts.seal).unwrap(),
        asked
    );
    // A real red is retained even if a later run supplies all-green events.
    begin_run(&f.root, &facts).unwrap();
    let red = RunEvents {
        finished: vec![finished("fmt", FAIL)],
        ..RunEvents::default()
    };
    write_run(
        &f.root,
        &facts,
        RunEnd {
            exit: 1,
            log_sha256: "",
            events: Ok(&red),
        },
    )
    .unwrap();
    assert!(input_missing(&f.root, TREE, facts.seal).is_err());
    write_run(
        &f.root,
        &facts,
        RunEnd {
            exit: 0,
            log_sha256: "",
            events: Ok(&events),
        },
    )
    .unwrap();
    assert!(input_missing(&f.root, TREE, facts.seal).is_err());
}

#[test]
fn input_items_round_trip_bounded_events_and_reject_missing_parts() {
    let f = Fixture::new();
    std::fs::create_dir_all(&f.root).unwrap();
    let at = f.root.join("input-events");
    let mut ctx = aterm_verify::Ctx::new(
        f.root.clone(),
        aterm_verify::Scope::workspace(),
        aterm_verify::EnvSnapshot::default(),
        f.root.join("scratch"),
    );
    ctx.carry_context = Some("c".repeat(64));
    ctx.events = Some(aterm_verify::event_log::EventLog::open(&at).unwrap());
    let proof = aterm_verify::input_content::Proof::new(
        &ctx,
        "census",
        "many-inputs",
        (0..1000)
            .map(|i| ("file".into(), format!("file-{i}"), "a".repeat(64)))
            .collect(),
    )
    .unwrap();
    aterm_verify::input_content::event(&ctx, &proof, false);
    let complete = events(&at).unwrap();
    assert!(complete.whole());
    assert_eq!(
        complete.input_items.as_slice(),
        std::slice::from_ref(&proof)
    );
    let text = std::fs::read_to_string(&at).unwrap();
    std::fs::write(
        &at,
        text.lines().skip(1).collect::<Vec<_>>().join("\n") + "\n",
    )
    .unwrap();
    let incomplete = events(&at).unwrap();
    assert!(!incomplete.whole());
    assert!(incomplete.input_items.is_empty());
}

#[test]
fn appkit_group_events_round_trip_and_malformed_proofs_fail_closed() {
    let f = Fixture::new();
    std::fs::create_dir_all(&f.root).unwrap();
    let at = f.root.join("appkit-events");
    let proof = appkit_redraw_group(7, 8);
    let event = aterm_json::json!({
        "schema": 1,
        "event": "appkit-group-finished",
        "group": proof.group,
        "content_key": proof.content_key,
        "executables": proof.executables,
        "reused": false,
    });
    std::fs::write(&at, format!("verify-event: {event}\n")).unwrap();
    let read = events(&at).unwrap();
    assert!(read.whole());
    assert_eq!(read.appkit_groups, [appkit_redraw_group(7, 8)]);

    let malformed = aterm_json::json!({
        "schema": 1,
        "event": "appkit-group-finished",
        "group": "redraw-conformance",
        "content_key": u(7),
        "executables": [{"name":"not-the-redraw-target","sha256":u(8)}],
        "reused": true,
    });
    std::fs::write(&at, format!("verify-event: {malformed}\n")).unwrap();
    let read = events(&at).unwrap();
    assert!(
        !read.whole(),
        "a recognized malformed proof is lost evidence"
    );
    assert!(read.appkit_groups.is_empty());
}

#[test]
fn an_appkit_ok_record_keeps_its_executable_group_proofs() {
    let f = Fixture::new();
    let proof = appkit_redraw_group(9, 10);
    let run = RunEvents {
        finished: vec![finished("appkit", OK)],
        appkit_groups: vec![proof.clone()],
        ..RunEvents::default()
    };
    f.write("s1", &u(1), 0, &run);
    let Read::Record(record) = read(&path(&f.root, TREE, "s1", "appkit", None)).unwrap() else {
        panic!("AppKit record");
    };
    assert_eq!(record.outcome, OK);
    assert_eq!(record.appkit_groups, [proof]);
}

#[test]
fn input_package_candidates_require_a_matching_successful_a19_record() {
    let f = Fixture::new();
    let mut events = all_ok();
    let good = aterm_verify::input_content::Proof {
        check: "fmt".into(),
        item: "good".into(),
        key: u(1),
        inputs: Vec::new(),
    };
    events.input_items = vec![good.clone()];
    f.write("input", "", 0, &events);
    let carried = input_item_candidates(&f.root, TREE, "input").unwrap();
    assert_eq!(carried.as_slice(), std::slice::from_ref(&good));
    assert!(
        crate::input_record::previous_items(&f.root, "base")
            .unwrap()
            .is_empty()
    );
    crate::input_record::attempted(&f.root, "base", TREE, "input").unwrap();
    assert_eq!(
        crate::input_record::previous_items(&f.root, "base").unwrap(),
        carried
    );
    assert!(
        crate::input_record::previous_items(&f.root, "another-base")
            .unwrap()
            .is_empty()
    );
    let mut interrupted = f.facts("input", "");
    interrupted.tree = "3333333333333333333333333333333333333333";
    begin_run(&f.root, &interrupted).unwrap();
    crate::input_record::attempted(&f.root, "base", interrupted.tree, "input").unwrap();
    assert!(
        crate::input_record::previous_items(&f.root, "base")
            .unwrap()
            .is_empty(),
        "a newer begun attempt cannot revive the preceding tree's green packages"
    );
    crate::input_record::attempted(&f.root, "base", TREE, "input").unwrap();
    assert!(
        input_item_candidates(&f.root, TREE, "different seal")
            .unwrap()
            .is_empty()
    );
    let red = RunEvents {
        finished: vec![finished("fmt", FAIL)],
        input_items: vec![good],
        ..RunEvents::default()
    };
    f.write("input", "", 1, &red);
    assert!(
        input_item_candidates(&f.root, TREE, "input")
            .unwrap()
            .is_empty()
    );
    assert!(
        crate::input_record::previous_items(&f.root, "base")
            .unwrap()
            .is_empty()
    );
    f.end("input", "", 0, Ok(&events)).unwrap();
    assert!(
        input_item_candidates(&f.root, TREE, "input")
            .unwrap()
            .is_empty(),
        "saved red stays red"
    );
}

/// Compilation may uplift the verifier path. Both halves use their captured running
/// image, independent of the later file. Missing driver evidence refuses
/// reuse, and a receipt that was merely carried cannot be archived as a new measurement.
#[test]
fn measure_receipts_bind_the_running_driver_after_an_uplift() {
    let f = Fixture::new();
    let executable = u(1);
    f.begin("seal", &executable).unwrap();
    let compiled = RunEvents {
        harnesses: MEASURING_TARGETS
            .into_iter()
            .chain(["verify"])
            .map(|n| (n.into(), u(2)))
            .collect(),
        running_driver: Some(u(2)),
        ..RunEvents::default()
    };
    keep_harnesses(&f.root, TREE, "seal", &compiled).unwrap();
    let predicted = product_key(&f.root, TREE, "seal", &u(3)).unwrap().unwrap();
    let mut uplifted = RunEvents {
        harnesses: compiled.harnesses.clone(),
        running_driver: Some(u(2)),
        ..RunEvents::default()
    };
    uplifted.harnesses.insert("verify".into(), u(9));
    keep_harnesses(&f.root, TREE, "seal", &uplifted).unwrap();
    assert_eq!(
        product_key(&f.root, TREE, "seal", &u(3)).unwrap().unwrap(),
        predicted,
        "a later on-disk image does not replace the captured driver"
    );
    let mut measured = RunEvents {
        running_driver: Some(u(5)),
        finished: vec![finished("measure", OK)],
        ..RunEvents::default()
    };
    keep_harnesses(&f.root, TREE, "seal", &measured).unwrap();
    let actual = product_key(&f.root, TREE, "seal", &u(3)).unwrap().unwrap();
    assert_ne!(actual, predicted);
    let mut whole = all_ok();
    whole.running_driver = Some(u(5));
    f.end("seal", &executable, 0, Ok(&whole)).unwrap();
    let facts = f.facts("seal", &executable);
    remember_product(
        &f.root,
        &RunFacts {
            asked: &[],
            ..facts
        },
        &actual,
    )
    .unwrap();
    let at = f
        .root
        .join("product-measure")
        .join(format!("{actual}.toml"));
    assert!(!at.exists(), "no measure was asked for");
    remember_product(&f.root, &facts, &actual).unwrap();
    assert!(at.exists());
    let carried = RunFacts {
        run: "another run",
        ..facts
    };
    let other = u(8);
    remember_product(&f.root, &carried, &other).unwrap();
    assert!(
        !f.root
            .join("product-measure")
            .join(format!("{other}.toml"))
            .exists()
    );
    measured.running_driver = None;
    keep_harnesses(&f.root, TREE, "seal", &measured).unwrap();
    assert!(product_key(&f.root, TREE, "seal", &u(3)).unwrap().is_none());
    let events_file = f._dir.path().join("driver.events");
    let lines: String = [u(2), u(5)]
        .into_iter()
        .map(|sha| {
            format!(
                "verify-event: {}\n",
                aterm_json::json!({"schema":1,"event":"measure-driver","sha256":sha})
            )
        })
        .collect();
    std::fs::write(&events_file, lines).unwrap();
    assert_eq!(
        events(&events_file).unwrap().running_driver.as_deref(),
        Some("")
    );
}

/// Lint-package receipts are last-writer-wins tombstones, and only a canonical bounded OK
/// can become carry evidence. Negative controls: a later unbounded row replaces an earlier
/// OK, a malformed claimed key is forced unbounded, and FAIL never carries.
#[test]
fn lint_package_events_fail_closed_and_keep_the_last_row() {
    let f = Fixture::new();
    let at = f._dir.path().join("lint-packages.events");
    let event =
        |spec: &str, outcome: &str, key: Option<String>, bounded: bool, reason: Option<&str>| {
            format!(
                "verify-event: {}\n",
                aterm_json::json!({
                    "schema": 1,
                    "event": "lint-package-finished",
                    "spec": spec,
                    "outcome": outcome,
                    "content_key": key,
                    "inputs_bounded": bounded,
                    "input_reason": reason,
                    "execution_s": 1.0,
                    "carried": spec == "d@1"
                })
            )
        };
    let text = [
        event("a@1", OK, Some(u(1)), true, None),
        event("a@1", NOT_RUN, None, false, Some("input drift")),
        event("b@1", OK, Some("not-a-key".into()), true, None),
        event("c@1", FAIL, Some(u(3)), true, None),
        event("d@1", OK, Some(u(4)), true, None),
    ]
    .concat();
    std::fs::write(&at, text).unwrap();
    let rows = events(&at).unwrap().lint_packages;
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].spec, "a@1");
    assert!(!rows[0].carried(), "the later tombstone wins");
    assert_eq!(rows[1].spec, "b@1");
    assert!(!rows[1].inputs_bounded);
    assert!(
        rows[1]
            .input_reason
            .as_deref()
            .is_some_and(|why| why.contains("non-canonical"))
    );
    assert!(!rows[2].carried(), "a prior FAIL is never carry evidence");
    assert!(rows[3].carried());
    assert!(
        rows[3].reused,
        "a reused verdict remains identified in the cut's record"
    );
    assert!(!rows[2].reused);
}

/// Runtime test failure keeps the product; a compile failure, another red, or another
/// seal cannot. Replacing this evidence check with `test == FAIL` fails the controls.
#[test]
fn only_a_proven_failed_test_executable_keeps_the_running_build() {
    for (binaries, other_red, expected) in [
        (true, false, true),
        (false, false, false),
        (true, true, false),
    ] {
        let f = Fixture::new();
        let mut events = all_ok();
        events
            .finished
            .retain(|e| e["check"].as_str() != Some("test"));
        events.finished.push(finished("test", FAIL));
        if binaries {
            events.binaries.push(bin("p::lib", 1, FAIL));
        }
        if other_red {
            events
                .finished
                .retain(|e| e["check"].as_str() != Some("lint"));
            events.finished.push(finished("lint", FAIL));
        }
        f.write("seal", &u(1), 1, &events);
        assert_eq!(
            keep_build_for_records(&f.root, COMMIT, TREE, "seal").unwrap(),
            expected
        );
        assert!(!keep_build_for_records(&f.root, COMMIT, TREE, "another seal").unwrap());
    }
}

/// A test-only tree shares the product verdict, while a changed U, harness or seal does
/// not. No tree check is carried. The FAIL control prevents a receipt from being rerolled.
#[test]
fn product_receipts_cross_trees_only_with_equal_content_harnesses_and_seal() {
    let f = Fixture::new();
    let executable = u(1);
    let mut events = all_ok();
    events.harnesses = MEASURING_TARGETS
        .into_iter()
        .chain(["verify"])
        .map(|name| (name.to_owned(), u(2)))
        .collect();
    events.running_driver = Some(u(2));
    f.write("seal", &executable, 0, &events);
    keep_harnesses(&f.root, TREE, "seal", &events).unwrap();
    let content = u(3);
    let product = product_key(&f.root, TREE, "seal", &content)
        .unwrap()
        .unwrap();
    remember_product(&f.root, &f.facts("seal", &executable), &product).unwrap();
    let other = "3333333333333333333333333333333333333333";
    std::fs::create_dir_all(f.root.join(other).join(seal_dir("seal"))).unwrap();
    keep_harnesses(&f.root, other, "seal", &events).unwrap();
    assert_eq!(
        product_key(&f.root, other, "seal", &content)
            .unwrap()
            .as_ref(),
        Some(&product)
    );
    let new_executable = u(4);
    let facts = RunFacts {
        tree: other,
        executable: Some(&new_executable),
        ..f.facts("seal", &executable)
    };
    assert!(reuse_product(&f.root, &facts, &product).unwrap());
    let judged = checks_in(&f.root, COMMIT, other, Some(&new_executable), || {
        Ok("seal".to_owned())
    })
    .unwrap();
    assert!(!judged.missing.iter().any(|name| name == "measure"));
    assert!(judged.missing.iter().any(|name| name == "test"));
    let changed = product_key(&f.root, other, "seal", &u(9)).unwrap().unwrap();
    assert_ne!(changed, product);
    assert!(!reuse_product(&f.root, &facts, &changed).unwrap());
    events.harnesses.insert("paint".into(), u(8));
    keep_harnesses(&f.root, other, "seal", &events).unwrap();
    assert_ne!(
        product_key(&f.root, other, "seal", &content)
            .unwrap()
            .as_ref(),
        Some(&product)
    );
    assert!(
        product_key(&f.root, other, "another seal", &content)
            .unwrap()
            .is_none()
    );
    let at = path(&f.root, other, "seal", "measure", Some(&new_executable));
    let Read::Record(mut fail) = read(&at).unwrap() else {
        panic!("measure receipt")
    };
    fail.outcome = FAIL.into();
    write(&at, &fail).unwrap();
    assert!(!reuse_product(&f.root, &facts, &product).unwrap());
    assert!(matches!(read(&at).unwrap(), Read::Record(r) if r.outcome == FAIL));
    fail.outcome = ACCEPTED.into();
    fail.acceptance = Some(Acceptance {
        reason: "owner accepted this measure".into(),
        failed_log_sha256: u(9),
    });
    write(&at, &fail).unwrap();
    assert!(!reuse_product(&f.root, &facts, &product).unwrap());
    assert!(
        matches!(read(&at).unwrap(), Read::Record(r) if r.outcome == ACCEPTED),
        "content reuse must not revoke an owner's acceptance"
    );
}

/// Every check ok on these bytes under this seal is complete; another seal, another
/// executable or a garbled record is missing, never an error, and so is a record of another
/// seal found where this seal's lies; a record that cannot be read leaves no answer; with no
/// records of the tree the seal is not taken. Negative control: a judge that skips the key
/// comparison passes the record moved under the other seal.
#[test]
fn a_tree_passes_only_on_its_own_records_under_its_seal() {
    let f = Fixture::new();
    let untaken = checks_in(&f.root, COMMIT, TREE, Some(&u(1)), || {
        Err("the seal is not taken with no records".into())
    })
    .unwrap();
    assert_eq!(untaken.missing, release_names());
    f.write("s1", &u(1), 0, &all_ok());
    let judged = f.judge("s1", Some(&u(1)));
    assert!(judged.complete(), "{}", judged.line());
    let other = f.judge("s2", Some(&u(1)));
    assert_eq!(other.missing, release_names(), "{}", other.line());
    let moved = path(&f.root, TREE, "s2", "fmt", None);
    std::fs::create_dir_all(moved.parent().unwrap()).unwrap();
    std::fs::copy(path(&f.root, TREE, "s1", "fmt", None), &moved).unwrap();
    assert_eq!(
        f.judge("s2", Some(&u(1))).missing,
        release_names(),
        "a record answers only for its own key, wherever it lies"
    );
    let rebuilt = f.judge("s1", Some(&u(2)));
    assert_eq!(
        rebuilt.missing,
        ["measure"],
        "tree records stand for another build"
    );
    assert_eq!(f.judge("s1", None).missing, ["measure"]);
    let lint = path(&f.root, TREE, "s1", "lint", None);
    std::fs::write(&lint, "not a record").unwrap();
    assert_eq!(f.judge("s1", Some(&u(1))).missing, ["lint"]);
    std::fs::remove_file(&lint).unwrap();
    std::fs::create_dir(&lint).unwrap();
    assert!(
        checks_in(&f.root, COMMIT, TREE, Some(&u(1)), || Ok("s1".into())).is_err(),
        "an unreadable record is no answer"
    );
}

/// A tree half judges no executable, so it keeps no record of a bundle check its events
/// name: measure's record always names the bytes it judged, and the tree's records stand
/// without one. Negative control: the same events with the executable keep measure under it.
#[test]
fn a_tree_half_keeps_no_bundle_record() {
    let f = Fixture::new();
    let facts = RunFacts {
        executable: None,
        ..f.facts("s1", "")
    };
    begin_run(&f.root, &facts).unwrap();
    let lines = write_run(
        &f.root,
        &facts,
        RunEnd {
            exit: 0,
            log_sha256: &u(9),
            events: Ok(&all_ok()),
        },
    )
    .unwrap();
    assert!(lines.iter().all(|l| !l.starts_with("measure")), "{lines:?}");
    assert_eq!(f.judge("s1", Some(&u(1))).missing, ["measure"]);
    f.write("s1", &u(1), 0, &all_ok());
    assert!(
        f.judge("s1", Some(&u(1))).complete(),
        "the control keeps it"
    );
}

/// A FAIL record refuses the tree under its seal, and a measure FAIL of another build of the
/// tree refuses it too; NOT RUN and skip are missing, and a resume runs only those, carrying
/// the test binaries that passed with a known digest. Negative controls: dropping the bundle
/// scan lets a rebuilt executable re-roll the measure FAIL; dropping the digest filter
/// carries a binary with none, which `--passed-binaries` refuses.
#[test]
fn a_fail_refuses_its_tree_and_a_resume_runs_only_what_is_missing() {
    let f = Fixture::new();
    let mut run = all_ok();
    run.finished = vec![
        finished("fmt", OK),
        finished("lint", OK),
        finished("doc", SKIP),
        finished("census", OK),
        finished("measure", FAIL),
        finished("cells", FAIL),
    ];
    run.binaries = vec![
        bin("-p a --lib", 1, OK),
        bin("-p b --lib", 2, NOT_RUN),
        Binary {
            sha256: String::new(),
            ..bin("-p c --lib", 3, OK)
        },
    ];
    let lines = f.write("s1", &u(1), 1, &run);
    assert_eq!(
        lines.len(),
        6,
        "five checks and the unfinished test: {lines:?}"
    );
    let judged = f.judge("s1", Some(&u(1)));
    assert_eq!(names(&judged.failed), ["measure"]);
    let refusal = judged.fail_refusal(COMMIT).expect("refused");
    assert!(!refusal.could_not_run);
    assert!(
        refusal.remedy.contains("a new hop changes the tree"),
        "{}",
        refusal.remedy
    );
    assert_eq!(
        names(&f.judge("s1", Some(&u(2))).failed),
        ["measure"],
        "a rebuilt executable never re-rolls a measure FAIL"
    );
    assert!(
        f.judge("s2", Some(&u(2))).fail_refusal(COMMIT).is_none(),
        "another seal"
    );

    let f = Fixture::new();
    run.finished
        .retain(|e| e["check"].as_str() != Some("measure"));
    let lines = f.write("s1", &u(1), 3, &run);
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("test NOT RUN (from its binaries)")),
        "{lines:?}"
    );
    let judged = f.judge("s1", Some(&u(1)));
    assert!(judged.failed.is_empty() && !judged.complete());
    let want: Vec<String> = release_names()
        .into_iter()
        .filter(|c| !["fmt", "lint", "census"].contains(&c.as_str()))
        .collect();
    assert_eq!(judged.missing, want);
    let resume = judged.run(Half::Tree);
    let tree_missing: Vec<String> = judged
        .missing
        .iter()
        .filter(|m| RELEASE_TREE.iter().any(|c| c.name() == m.as_str()))
        .cloned()
        .collect();
    assert_eq!(resume.checks, tree_missing);
    assert_eq!(
        resume.passed_binaries,
        [bin("-p a --lib", 1, OK)],
        "a binary whose digest is not known runs again"
    );
    assert!(resume.strict_subset() && !resume.reads_the_bundle());
    let bundle = judged.run(Half::Bundle);
    assert_eq!(bundle.checks, ["appkit", "measure"]);
    assert!(!bundle.strict_subset() && bundle.reads_the_bundle());
    assert!(bundle.passed_binaries.is_empty(), "test is the tree's");
    let all = VerifyRun {
        half: Half::Tree,
        checks: RELEASE_TREE.iter().map(|c| c.name().to_owned()).collect(),
        passed_binaries: Vec::new(),
        passed_lint_packages: Vec::new(),
        passed_appkit_groups: Vec::new(),
    };
    assert!(!all.strict_subset());
    let tree_only = VerifyRun {
        half: Half::Tree,
        checks: vec!["lint".into(), "test".into()],
        passed_binaries: Vec::new(),
        passed_lint_packages: Vec::new(),
        passed_appkit_groups: Vec::new(),
    };
    assert!(tree_only.strict_subset() && !tree_only.reads_the_bundle());
}

/// An interrupted lint run resumes with only canonical bounded package OKs. An unbounded
/// reader stays a tombstone and is not offered; the bundle half never receives lint proofs.
#[test]
fn a_lint_resume_offers_only_bounded_package_oks() {
    let f = Fixture::new();
    let mut events = all_ok();
    events
        .finished
        .retain(|event| event["check"].as_str() != Some("lint"));
    let ok = lint_package("clean@1", 1, OK);
    let mut unbounded = lint_package("reader@1", 2, NOT_RUN);
    unbounded.inputs_bounded = false;
    unbounded.content_key = None;
    unbounded.input_reason = Some("undeclared proc macro reader".into());
    events.lint_packages = vec![ok.clone(), unbounded];
    f.write("s1", &u(1), 3, &events);

    let judged = f.judge("s1", Some(&u(1)));
    assert!(judged.failed.is_empty());
    assert!(judged.missing.iter().any(|check| check == "lint"));
    assert_eq!(judged.lint_resume, std::slice::from_ref(&ok));
    let tree = judged.run(Half::Tree);
    assert_eq!(tree.passed_lint_packages, [ok]);
    assert!(judged.run(Half::Bundle).passed_lint_packages.is_empty());
}

/// A test check killed before it finished is recorded from its binaries (FAIL when one
/// failed), and an exit 1 whose events name no FAIL, or could not be read, is a run-level FAIL
/// of its own seal alone, so the FAIL rule never rests on parsing; a run cannot begin over it.
/// Negative controls: without the run-level record the exit-1 tree reads as merely missing; a
/// run-level judge that skips the key refuses the tree under the seal the record was moved to.
#[test]
fn a_fail_is_recorded_whatever_its_events_let_the_cutter_read() {
    let f = Fixture::new();
    let run = RunEvents {
        binaries: vec![bin("-p a --lib", 1, OK), bin("-p b --lib", 2, FAIL)],
        ..RunEvents::default()
    };
    f.write("s1", &u(1), 1, &run);
    assert_eq!(names(&f.judge("s1", Some(&u(1))).failed), ["test"]);

    let f = Fixture::new();
    let lines = f.write("s1", &u(1), 1, &RunEvents::default());
    assert_eq!(lines.len(), 1, "{lines:?}");
    let judged = f.judge("s1", Some(&u(1)));
    assert_eq!(names(&judged.failed), [RUN_LEVEL]);
    assert!(!judged.failed[0].unrecorded);
    let other = f.judge("s2", Some(&u(1)));
    assert!(other.failed.is_empty(), "{}", other.line());
    assert_eq!(other.missing, release_names());
    let moved = path(&f.root, TREE, "s2", RUN_LEVEL, None);
    std::fs::create_dir_all(moved.parent().unwrap()).unwrap();
    std::fs::copy(path(&f.root, TREE, "s1", RUN_LEVEL, None), &moved).unwrap();
    assert!(
        f.judge("s2", Some(&u(1))).failed.is_empty(),
        "a run-level FAIL answers only for its own seal"
    );
    assert!(
        f.begin("s1", &u(1)).is_err(),
        "no run begins over a FAIL of its tree and seal"
    );

    let f = Fixture::new();
    f.begin("s1", &u(1)).unwrap();
    assert!(
        f.end("s1", &u(1), 1, Err("its events file is gone"))
            .is_err()
    );
    assert_eq!(
        names(&f.judge("s1", Some(&u(1))).failed),
        [RUN_LEVEL],
        "an exit 1 whose events could not be read still refuses"
    );
    let f = Fixture::new();
    assert!(f.write("s1", &u(1), 3, &RunEvents::default()).is_empty());
    assert!(f.judge("s1", Some(&u(1))).failed.is_empty());
}

/// The FAIL rule never rests on a write: a run reads as FAILED from before it starts until
/// every FAIL it showed is kept. A FAIL whose record cannot be written leaves the `began`
/// record, which refuses the tree; FAILs are written before any other record, so one other
/// record that cannot be written loses no FAIL; a killed run whose events were not read whole
/// keeps `began`, and one read whole settles to what they say; an exit 3 settles on verify's
/// own word; a run that no longer judged the tree withdraws its `began`; and no later run
/// replaces a FAIL. Negative controls: without the `began` record a FAIL that could not be
/// written leaves the tree merely missing; writing a FAIL after the others loses it to the
/// first that fails; a plain write in place of `keep` replaces the FAIL with an ok.
#[test]
fn a_fail_that_cannot_be_written_still_refuses_its_tree() {
    let block = |at: &Path| std::fs::create_dir_all(at).unwrap();
    let unblock = |at: &Path| std::fs::remove_dir(at).unwrap();
    let test_fails = RunEvents {
        finished: vec![finished("lint", OK), finished("test", FAIL)],
        ..RunEvents::default()
    };

    let f = Fixture::new();
    f.begin("s1", &u(1)).unwrap();
    let judged = f.judge("s1", Some(&u(1)));
    assert_eq!(names(&judged.failed), [RUN_LEVEL], "a run under way");
    assert!(judged.failed[0].unrecorded);
    let test = path(&f.root, TREE, "s1", "test", None);
    block(&test);
    assert!(f.end("s1", &u(1), 1, Ok(&test_fails)).is_err());
    unblock(&test);
    let judged = f.judge("s1", Some(&u(1)));
    assert_eq!(names(&judged.failed), [RUN_LEVEL], "{}", judged.line());
    assert!(judged.fail_refusal(COMMIT).is_some());

    let f = Fixture::new();
    f.begin("s1", &u(1)).unwrap();
    let lint = path(&f.root, TREE, "s1", "lint", None);
    block(&lint);
    assert!(f.end("s1", &u(1), 1, Ok(&test_fails)).is_err());
    unblock(&lint);
    let judged = f.judge("s1", Some(&u(1)));
    assert_eq!(
        names(&judged.failed),
        ["test"],
        "the FAIL was written first: {}",
        judged.line()
    );

    let f = Fixture::new();
    let killed = RunEvents {
        binaries: vec![bin("-p a --lib", 1, OK)],
        partial: true,
        ..RunEvents::default()
    };
    f.begin("s1", &u(1)).unwrap();
    f.end("s1", &u(1), -1, Ok(&killed)).unwrap();
    assert_eq!(names(&f.judge("s1", Some(&u(1))).failed), [RUN_LEVEL]);
    let f = Fixture::new();
    f.begin("s1", &u(1)).unwrap();
    assert!(f.end("s1", &u(1), 137, Err("unreadable")).is_err());
    assert_eq!(names(&f.judge("s1", Some(&u(1))).failed), [RUN_LEVEL]);
    let f = Fixture::new();
    let whole = RunEvents {
        partial: false,
        ..killed
    };
    f.write("s1", &u(1), -1, &whole);
    let judged = f.judge("s1", Some(&u(1)));
    assert!(judged.failed.is_empty(), "{}", judged.line());
    assert_eq!(
        judged.run(Half::Tree).passed_binaries,
        [bin("-p a --lib", 1, OK)]
    );

    let f = Fixture::new();
    f.begin("s1", &u(1)).unwrap();
    let lint = path(&f.root, TREE, "s1", "lint", None);
    block(&lint);
    assert!(f.end("s1", &u(1), 3, Ok(&all_ok())).is_err());
    unblock(&lint);
    let judged = f.judge("s1", Some(&u(1)));
    assert!(judged.failed.is_empty(), "{}", judged.line());
    assert_eq!(judged.missing, ["lint"], "an exit 3 found no FAIL");

    let f = Fixture::new();
    f.begin("s1", &u(1)).unwrap();
    abandon_run(&f.root, &f.facts("s1", &u(1))).unwrap();
    assert_eq!(f.judge("s1", Some(&u(1))).missing, release_names());

    let f = Fixture::new();
    f.write("s1", &u(1), 1, &test_fails);
    let lines = f.write(
        "s1",
        &u(1),
        0,
        &RunEvents {
            finished: vec![finished("test", OK)],
            ..RunEvents::default()
        },
    );
    assert!(
        lines.iter().any(|l| l.starts_with("test ok not kept")),
        "{lines:?}"
    );
    assert_eq!(names(&f.judge("s1", Some(&u(1))).failed), ["test"]);
}

/// The immediately preceding hop may supply only equal whole-check OKs and bounded binary
/// OKs. A binary FAIL and a legacy sha-only OK never carry. Evidence already recorded on the
/// current hop wins over an older candidate, so a second facts pass cannot erase progress.
#[test]
fn a_hop_carries_equal_ok_content_and_never_a_fail_or_unbounded_reader() {
    const PREVIOUS_TREE: &str = "3333333333333333333333333333333333333333";
    const PREVIOUS_COMMIT: &str = "4444444444444444444444444444444444444444";
    const SEAL: &str = "sealed";

    let f = Fixture::new();
    let input = |key: Option<String>, declaration: &str| crate::check_content::Input {
        context: "same-context".into(),
        key,
        declaration: declaration.into(),
    };
    let fmt_input = input(Some(u(40)), "fmt inputs");
    let old_test_input = input(Some(u(41)), "old test tree");
    let new_test_input = input(Some(u(42)), "new test tree");
    let appkit_input = crate::check_content::Input {
        context: "bundle-context".into(),
        key: None,
        declaration: "appkit executable groups are re-keyed after their current build".into(),
    };
    let mut planned = BTreeMap::new();
    planned.insert("appkit".into(), appkit_input.clone());
    planned.insert("fmt".into(), fmt_input.clone());
    planned.insert("test".into(), new_test_input.clone());
    let plan = crate::check_content::Plan {
        context: "same-context".into(),
        checks: planned,
    };

    let record = |tree: &str,
                  commit: &str,
                  check: &str,
                  outcome: &str,
                  content: crate::check_content::Input,
                  binaries: Vec<Binary>| CheckRecord {
        schema: SCHEMA,
        check: check.into(),
        key: key(check, tree, SEAL, None),
        tree: tree.into(),
        commit: commit.into(),
        seal: SEAL.into(),
        content: Some(content),
        executable: None,
        outcome: outcome.into(),
        seconds: 1.0,
        detail: String::new(),
        rows: Vec::new(),
        log: f.log.display().to_string(),
        log_sha256: u(9),
        run: RUN.into(),
        exit: i32::from(outcome == FAIL),
        acceptance: None,
        binaries,
        lint_packages: Vec::new(),
        appkit_groups: Vec::new(),
        input_items: Vec::new(),
    };
    let keep_record = |record: &CheckRecord| {
        let at = path(&f.root, &record.tree, SEAL, &record.check, None);
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        write(&at, record).unwrap();
    };

    keep_record(&record(
        PREVIOUS_TREE,
        PREVIOUS_COMMIT,
        "fmt",
        OK,
        fmt_input,
        Vec::new(),
    ));
    let mut prior_appkit = record(
        PREVIOUS_TREE,
        PREVIOUS_COMMIT,
        "appkit",
        OK,
        appkit_input,
        Vec::new(),
    );
    prior_appkit.appkit_groups = vec![appkit_redraw_group(43, 44)];
    keep_record(&prior_appkit);
    let prior_ok = bin("-p p --test reader", 1, OK);
    let prior_drift = bin("-p p --test drifted", 5, OK);
    let prior_fail = bin("-p p --test failed", 2, FAIL);
    let mut legacy_ok = bin("-p p --test legacy", 3, OK);
    legacy_ok.content_key = None;
    legacy_ok.inputs_bounded = false;
    legacy_ok.input_reason = None;
    keep_record(&record(
        PREVIOUS_TREE,
        PREVIOUS_COMMIT,
        "test",
        FAIL,
        old_test_input,
        vec![prior_ok, prior_drift, prior_fail, legacy_ok],
    ));
    let prior_lint_ok = lint_package("clean@1.0.0", 10, OK);
    let prior_lint_drift = lint_package("drift@1.0.0", 11, OK);
    let prior_lint_fail = lint_package("failed@1.0.0", 12, FAIL);
    let mut prior_lint_unbounded = lint_package("reader@1.0.0", 13, OK);
    prior_lint_unbounded.inputs_bounded = false;
    prior_lint_unbounded.content_key = None;
    prior_lint_unbounded.input_reason = Some("undeclared build script reader".into());
    let mut prior_lint = record(
        PREVIOUS_TREE,
        PREVIOUS_COMMIT,
        "lint",
        FAIL,
        input(None, "lint is carried per package"),
        Vec::new(),
    );
    prior_lint.lint_packages = vec![
        prior_lint_ok.clone(),
        prior_lint_drift,
        prior_lint_fail,
        prior_lint_unbounded,
    ];
    keep_record(&prior_lint);

    let mut current_reader = bin("-p p --test reader", 1, OK);
    current_reader.content_key = Some(u(55));
    let current_sibling = bin("-p p --test sibling", 4, OK);
    let mut current_drift = bin("-p p --test drifted", 5, NOT_RUN);
    current_drift.content_key = None;
    current_drift.inputs_bounded = false;
    current_drift.input_reason = Some("runtime inputs changed after carry selection".into());
    keep_record(&record(
        TREE,
        COMMIT,
        "test",
        NOT_RUN,
        new_test_input,
        vec![
            current_reader.clone(),
            current_sibling.clone(),
            current_drift.clone(),
        ],
    ));
    let mut current_lint_drift = lint_package("drift@1.0.0", 21, NOT_RUN);
    current_lint_drift.inputs_bounded = false;
    current_lint_drift.content_key = None;
    current_lint_drift.input_reason = Some("compile-time input changed after selection".into());
    let current_lint_sibling = lint_package("sibling@1.0.0", 22, OK);
    let mut current_lint = record(
        TREE,
        COMMIT,
        "lint",
        NOT_RUN,
        input(None, "lint is carried per package"),
        Vec::new(),
    );
    current_lint.lint_packages = vec![current_lint_drift.clone(), current_lint_sibling.clone()];
    keep_record(&current_lint);

    let carried = carry_hop(
        &f.root,
        COMMIT,
        TREE,
        PREVIOUS_COMMIT,
        PREVIOUS_TREE,
        SEAL,
        &plan,
    )
    .unwrap();
    assert_eq!(carried.carried_checks, ["fmt"]);
    assert!(carried.rerun_checks.iter().any(|check| check == "appkit"));
    assert!(carried.rerun_checks.iter().any(|check| check == "test"));

    let Read::Record(appkit) = read(&path(&f.root, TREE, SEAL, "appkit", None)).unwrap() else {
        panic!("current AppKit candidate record");
    };
    assert_eq!(
        appkit.outcome, NOT_RUN,
        "a proof candidate is not a verdict"
    );
    assert_eq!(appkit.appkit_groups, [appkit_redraw_group(43, 44)]);
    let judged = checks_in(&f.root, COMMIT, TREE, None, || Ok(SEAL.into())).unwrap();
    assert_eq!(judged.appkit_resume, [appkit_redraw_group(43, 44)]);
    let appkit_run = judged.run_only(Half::Bundle, &[Check::Appkit]);
    assert_eq!(appkit_run.checks, ["appkit"]);
    assert_eq!(
        appkit_run.passed_appkit_groups,
        [appkit_redraw_group(43, 44)]
    );

    let Read::Record(test) = read(&path(&f.root, TREE, SEAL, "test", None)).unwrap() else {
        panic!("current test candidate record");
    };
    assert_eq!(
        test.binaries,
        [
            current_drift.clone(),
            current_reader.clone(),
            current_sibling.clone()
        ],
        "current OKs win; current drift tombstones prior OK; FAIL and legacy rows stay absent"
    );
    let Read::Record(lint) = read(&path(&f.root, TREE, SEAL, "lint", None)).unwrap() else {
        panic!("current lint candidate record");
    };
    assert_eq!(
        lint.lint_packages,
        [
            prior_lint_ok.clone(),
            current_lint_drift.clone(),
            current_lint_sibling.clone()
        ],
        "only bounded package OKs carry; current tombstones win over prior OKs"
    );

    // A same-tree resume asks for hop carry again. The persisted NOT RUN row must still win
    // over the preceding tree's OK; dropping tombstones from the synthetic record resurrects
    // `prior_drift` here.
    carry_hop(
        &f.root,
        COMMIT,
        TREE,
        PREVIOUS_COMMIT,
        PREVIOUS_TREE,
        SEAL,
        &plan,
    )
    .unwrap();
    let Read::Record(test) = read(&path(&f.root, TREE, SEAL, "test", None)).unwrap() else {
        panic!("same-tree resumed test candidate record");
    };
    assert_eq!(
        test.binaries,
        [current_drift, current_reader, current_sibling],
        "a current NOT RUN tombstone survives reseeding and blocks the older OK"
    );
    let Read::Record(lint) = read(&path(&f.root, TREE, SEAL, "lint", None)).unwrap() else {
        panic!("same-tree resumed lint candidate record");
    };
    assert_eq!(
        lint.lint_packages,
        [prior_lint_ok, current_lint_drift, current_lint_sibling],
        "a current package tombstone blocks resurrection on a second carry pass"
    );

    // A later line hop may not relay this synthetic record's rows. None of them was re-keyed
    // on TREE: `carry_hop` merely offered the preceding hop's receipts to a verifier which has
    // not run. Removing the candidate-run provenance guard lets both test and lint rows cross
    // this second boundary without ever being judged on TREE.
    const NEXT_TREE: &str = "5555555555555555555555555555555555555555";
    const NEXT_COMMIT: &str = "6666666666666666666666666666666666666666";
    let next = carry_hop(&f.root, NEXT_COMMIT, NEXT_TREE, COMMIT, TREE, SEAL, &plan).unwrap();
    assert!(
        next.rerun_checks.iter().any(|check| check == "appkit")
            && next.rerun_checks.iter().any(|check| check == "test")
            && next.rerun_checks.iter().any(|check| check == "lint")
    );
    assert!(
        matches!(
            read(&path(&f.root, NEXT_TREE, SEAL, "appkit", None)).unwrap(),
            Read::Absent
        ),
        "an unexecuted AppKit candidate must not relay across two hops"
    );
    assert!(
        matches!(
            read(&path(&f.root, NEXT_TREE, SEAL, "test", None)).unwrap(),
            Read::Absent
        ),
        "an unexecuted candidate record must not relay binary proofs across two hops"
    );
    assert!(
        matches!(
            read(&path(&f.root, NEXT_TREE, SEAL, "lint", None)).unwrap(),
            Read::Absent
        ),
        "an unexecuted candidate record must not relay lint proofs across two hops"
    );

    // NEGATIVE CONTROL: a changed canonical AppKit context cannot carry executable candidates.
    const RELATED_TREE: &str = "7777777777777777777777777777777777777777";
    const RELATED_COMMIT: &str = "8888888888888888888888888888888888888888";
    let mut related_plan = plan.clone();
    related_plan.checks.insert(
        "appkit".into(),
        crate::check_content::Input {
            context: "changed-bundle-context".into(),
            key: None,
            declaration: "AppKit's executable groups require the current canonical context".into(),
        },
    );
    let related = carry_hop(
        &f.root,
        RELATED_COMMIT,
        RELATED_TREE,
        PREVIOUS_COMMIT,
        PREVIOUS_TREE,
        SEAL,
        &related_plan,
    )
    .unwrap();
    assert!(
        related.rerun_checks.iter().any(|check| check == "appkit")
            && !related.carried_checks.iter().any(|check| check == "appkit"),
        "a related hop must run AppKit despite an equal stored context key"
    );
    assert!(
        matches!(
            read(&path(&f.root, RELATED_TREE, SEAL, "appkit", None)).unwrap(),
            Read::Absent
        ),
        "a failed transient gate materializes no synthetic AppKit verdict"
    );

    let mut legacy = record(
        PREVIOUS_TREE,
        PREVIOUS_COMMIT,
        "test",
        FAIL,
        input(Some(u(60)), "legacy"),
        vec![bin("-p p --lib", 8, FAIL)],
    );
    legacy.content = None;
    let encoded = aterm_toml::to_string(&legacy).unwrap();
    let old = encoded
        .lines()
        .filter(|line| {
            ![
                "content_key",
                "inputs_bounded",
                "input_reason",
                "json",
                "execution_s",
            ]
            .iter()
            .any(|field| line.starts_with(field))
        })
        .collect::<Vec<_>>()
        .join("\n");
    let decoded: CheckRecord = aterm_toml::from_str(&old).unwrap();
    assert_eq!(decoded.outcome, FAIL);
    assert_eq!(decoded.binaries[0].outcome, FAIL);
    assert!(
        !decoded.binaries[0].carried(),
        "legacy evidence fails closed"
    );
}

#[test]
fn appkit_fail_and_malformed_group_proofs_never_seed_a_hop() {
    const PREVIOUS_COMMIT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const CURRENT_COMMIT: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const SEAL: &str = "appkit-seal";
    let input = crate::check_content::Input {
        context: "early-appkit-context".into(),
        key: None,
        declaration: "AppKit groups are re-keyed after build".into(),
    };
    let mut checks = BTreeMap::new();
    checks.insert("appkit".into(), input.clone());
    let plan = crate::check_content::Plan {
        context: "tree-context".into(),
        checks,
    };

    for (index, outcome, proof) in [
        (1_u8, FAIL, appkit_redraw_group(1, 2)),
        (
            2_u8,
            OK,
            aterm_verify::appkit_content::GroupProof {
                executables: vec![aterm_verify::appkit_content::Executable {
                    name: "wrong-target".into(),
                    sha256: u(3),
                }],
                ..appkit_redraw_group(3, 4)
            },
        ),
    ] {
        let f = Fixture::new();
        let previous_tree = format!("{index:02x}").repeat(20);
        let current_tree = format!("{:02x}", index + 16).repeat(20);
        let facts = RunFacts {
            commit: PREVIOUS_COMMIT,
            tree: &previous_tree,
            seal: SEAL,
            executable: None,
            log: &f.log,
            run: RUN,
            asked: &[],
            content: None,
        };
        let mut prior = record(
            &facts,
            (i32::from(outcome == FAIL), &u(9)),
            "appkit",
            outcome,
            None,
        );
        prior.content = Some(input.clone());
        prior.appkit_groups = vec![proof];
        let at = path(&f.root, &previous_tree, SEAL, "appkit", None);
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        write(&at, &prior).unwrap();

        carry_hop(
            &f.root,
            CURRENT_COMMIT,
            &current_tree,
            PREVIOUS_COMMIT,
            &previous_tree,
            SEAL,
            &plan,
        )
        .unwrap();
        assert!(
            matches!(
                read(&path(&f.root, &current_tree, SEAL, "appkit", None)).unwrap(),
                Read::Absent
            ),
            "{outcome} or malformed proof must not become a candidate"
        );
    }
}

/// A legacy single record (a file at `verified/<tree>`) stands for nothing and is replaced
/// by the next run's directory of records. Negative control: without the removal the run
/// cannot begin on the file in its path.
#[test]
fn a_legacy_single_record_is_missing_and_replaced() {
    let f = Fixture::new();
    std::fs::create_dir_all(&f.root).unwrap();
    std::fs::write(
        f.root.join(TREE),
        "commit = \"x\"\nexecutable = \"y\"\ninputs = \"z\"\n",
    )
    .unwrap();
    let judged = f.judge("s1", Some(&u(1)));
    assert_eq!(judged.missing, release_names(), "{}", judged.line());
    f.write("s1", &u(1), 0, &all_ok());
    assert!(f.root.join(TREE).is_dir());
    assert!(f.judge("s1", Some(&u(1))).complete());
}

/// An owner's acceptance rewrites only a measure FAIL of this tree under this seal, read
/// from the named report as it still is, and completes the tree only beside every other
/// check's ok record. Negative control: an acceptance that skips the seal comparison accepts
/// the FAIL of another seal.
#[test]
fn an_accepted_measure_passes_only_beside_every_other_ok() {
    let f = Fixture::new();
    let mut run = all_ok();
    run.finished.retain(|e| e["check"].as_str() != Some("test"));
    let report = std::fs::canonicalize(&f.log).unwrap();
    let accept = |seal: &str, report: &Path, sha: &str| {
        accept_in(
            &f.root,
            (COMMIT, TREE),
            &u(1),
            seal,
            (report, sha),
            "the owner shipped it",
        )
    };
    f.write("s1", &u(1), 0, &run);
    assert!(
        accept("s1", &report, &u(9)).is_err(),
        "an ok measure is not accepted"
    );
    run.finished
        .retain(|e| e["check"].as_str() != Some("measure"));
    run.finished.push(finished("measure", FAIL));
    f.write("s1", &u(1), 1, &run);
    assert!(f.judge("s1", Some(&u(1))).fail_refusal(COMMIT).is_some());
    assert!(accept("s2", &report, &u(9)).is_err(), "another seal");
    assert!(accept("s1", &report, &u(8)).is_err(), "a changed report");
    assert!(
        accept("s1", Path::new("/elsewhere/06-verify.log"), &u(9)).is_err(),
        "another report"
    );
    accept("s1", &report, &u(9)).unwrap();
    let judged = f.judge("s1", Some(&u(1)));
    assert_eq!(judged.missing, ["test"]);
    assert!(judged.accepted.is_none());
    f.write(
        "s1",
        &u(1),
        0,
        &RunEvents {
            finished: vec![finished("test", OK)],
            ..RunEvents::default()
        },
    );
    let judged = f.judge("s1", Some(&u(1)));
    assert!(judged.complete(), "{}", judged.line());
    assert!(judged.accepted.is_some());
}
