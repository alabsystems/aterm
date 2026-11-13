// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn log(text: &str) -> Measured {
    Measured::of_log(text.as_bytes(), "0")
}

#[test]
fn a_summary_line_parses_as_the_shell_read_it() {
    assert_eq!(
        parse_summary("  Summary: 3 proved, 0 failed, 0 assumed, 0 inconclusive (7 total)"),
        (Some(3), Some(7))
    );
    assert_eq!(
        parse_summary("Summary: 1 proved (2 total) Summary: 5 proved (9 total)"),
        (Some(5), Some(9))
    );
    assert_eq!(
        parse_summary("  Summary: three properties proved, nothing outstanding"),
        (None, None)
    );
    assert_eq!(
        parse_summary("Summary: 4 proved, no total"),
        (Some(4), None)
    );
}

#[test]
fn a_log_is_measured_as_the_shell_measured_it() {
    let m = log(concat!(
        "noise timed_out=3 and timed_out=4\n",
        "  [UNKNOWN] [unknown] unsupported MIR `MemoryAdmission`: memory admission failed before solver dispatch: configured trustd admission failed: could not connect to selected endpoint\n",
        "    [unsupported_mir] unsupported MIR `MemoryAdmission`: memory admission failed before solver dispatch: could not connect\n",
        "note: trust: memory census: 397 bodies verified and compacted, 2 compacted slots recomputed, 3 bodies skipped at the memory ceiling; resident 1.2 GiB, peak 3.5 GiB, ceiling 3.0 GiB\n",
        "  [UNKNOWN] [unknown] malformed: type lowering exceeded the per-function verification budget\n",
        "    [unsupported_mir] detail: type lowering exceeded the per-function verification budget\n",
        " ERROR ay_dpll::executor::proof::check::internal internal proof checker rejected UNSAT proof, error=proof validation cancelled by the caller (interrupt, deadline, or memory limit), checked_steps=6, skipped_hole_steps=0, total_steps=6\n",
        " ERROR ay_dpll::executor::proof::check::internal internal proof checker rejected UNSAT proof, error=step t9 has invalid resolution derivation, checked_steps=3, skipped_hole_steps=0, total_steps=9\n",
        "  Summary: 2 proved, 0 failed (3 total)\n",
    ));
    assert_eq!(
        (m.lapsed, m.cancelled),
        (7, 1),
        "seven timed out; one proof check cancelled, an invalid derivation not"
    );
    assert_eq!(m.refused, 1, "the [UNKNOWN] listing, not its detail line");
    assert_eq!(m.refused_why, "trustd unreachable: 1");
    assert_eq!(m.skipped, Some(3));
    assert_eq!(m.ceiling, "3.0 GiB");
    assert_eq!(m.budget_rows, 1, "result rows only, never the detail line");
    assert!(!m.transport);
    assert_eq!(parse_summary(&m.summary), (Some(2), Some(3)));
    assert_eq!(m.peak, "3.5 GiB");
    let m = log("rustc -Ztrust-verify-memory-ceiling-bytes=3221225472 x\n");
    assert_eq!(m.ceiling, "3221225472 B");
    assert_eq!(gib_of(&m.ceiling), Some(3.0));
    let m = log(concat!(
        "note: trust: memory census: 12 artifact slots compacted after their verify pass (0 after the pass had dropped its own handle), 0 compacted slots recomputed by a later pass; resident 1.1 GiB now, highest 2.3 GiB read at a body boundary\n",
        "  Summary: 2 proved, 0 failed (2 total)\n",
    ));
    assert_eq!(
        (m.skipped, m.ceiling.as_str(), m.peak.as_str()),
        (None, "", "")
    );
    let fields = m.to_fields();
    assert_eq!(fields.split('\t').nth(6), Some("-"), "{fields}");
    let back = Measured::from_fields(&fields.split('\t').collect::<Vec<_>>());
    assert_eq!(back.map(|(b, _)| b.skipped), Some(None));
    assert!(log("targo trust: error: verification did not run\n").transport);
}

const DONE: &str = "Summary: 5 proved, 3 failed, 8 runtime-checked, 0 assumed, 0 mandated, 0 contract-panic, 5 inconclusive (21 total)\n\
                    Coverage: 9/9 eligible function bodies verified (complete)\n\
                    \x20 Gate: FAIL [advisory lane, exit 1] (5 proved / 21 total)\n";

const KILLED: &str = "error: could not compile `aterm-census` (lib); 216 warnings emitted\n\nCaused by:\n\
                      \x20 process didn't exit successfully: `/Users//x/toolchains/trust-6606d3bd/bin/trustc --crate-name aterm_census --edition=2024 crates/aterm-census/src/lib.rs --crate-type lib` (signal: 15, SIGTERM: termination signal)\n\
                      === Trust Verification Report ===\n\
                      Level: L2 | Solver timeout: 5000ms | Function budget: 120000ms | Duration: 7865903ms\n\
                      \x20 [UNKNOWN] [transport:missing-crate-summary] compiler emitted function rows without an end-of-compile target inventory (targo-trust)\n\
                      Summary: 207 proved, 440 failed, 252 runtime-checked, 56 assumed, 9 mandated, 0 contract-panic, 1073 inconclusive (2037 total)\n\
                      Coverage: unknown (compiler emitted no coverage_summary transport row; older toolchain?)\n\
                      \x20   [assertion] assertion: transport:missing-crate-summary UNKNOWN (targo-trust: missing crate_summary transport prevents proof that this rustc target completed)\n\
                      \x20 Gate: FAIL [advisory lane, exit 101] (207 proved / 2037 total)\n";

#[test]
fn a_check_that_did_not_finish_says_so_in_its_own_record() {
    let ended = |text: &str, rc: &str| Measured::of_log(text.as_bytes(), rc).ended;
    let m = Measured::of_log(KILLED.as_bytes(), "101");
    assert_eq!(
        parse_summary(&m.summary),
        (Some(207), Some(2037)),
        "its Summary still parses"
    );
    assert!(!m.transport);
    assert!(!m.ended.is_empty());
    for rc in ["0", "1", "2", "101", "?"] {
        assert_eq!(ended(DONE, rc), "", "rc {rc}");
    }
    assert_eq!(ended("", "0"), "", "no Summary is GATE-BROKE's, not this");
    for kind in [
        "missing-crate-summary",
        "summary-accounting",
        "crate-summary-accounting",
        "ordering",
        "unsupported-message",
        "malformed",
    ] {
        assert!(
            !ended(
                &format!("  [UNKNOWN] [transport:{kind}] the {kind} defect (targo-trust)\n{DONE}"),
                "1"
            )
            .is_empty(),
            "{kind}"
        );
    }
    let m = Measured::of_log(
        format!("  [UNKNOWN] [transport:missing-json] missing structured Trust JSON transport (targo-trust)\n{DONE}")
            .as_bytes(),
        "2",
    );
    assert!(
        m.transport && m.ended.is_empty(),
        "missing-json is `transport`: {m:?}"
    );
    assert!(
        !ended(
            &format!(
                "  process didn't exit successfully: `/x/bin/trustc --crate-name a` (signal: 9, SIGKILL: kill)\n{DONE}"
            ),
            "101"
        )
        .is_empty()
    );
    assert_eq!(
        ended(
            &format!(
                "  process didn't exit successfully: `/x/bin/trustc --crate-name a` (exit status: 1)\n{DONE}"
            ),
            "101"
        ),
        ""
    );
    for rc in ["143", "137", "158"] {
        assert!(!ended(DONE, rc).is_empty(), "rc {rc}");
    }
    assert_eq!(ended(DONE, "128"), "");
    assert_eq!(ended(DONE, "193"), "");
    let summary = "Summary: 5 proved (21 total)\n";
    assert!(!ended(summary, "0").is_empty());
    assert!(
        !ended(&format!("{summary}Coverage: 9/12 — coverage shortfall: 3 function(s) were never verified (fail-closed: never a passing gate)\n"), "1")
            .is_empty()
    );
    assert!(
        !ended(
            &format!("{summary}Coverage: 8/9 eligible function bodies verified (complete)\n"),
            "0"
        )
        .is_empty(),
        "counts that disagree are no complete coverage"
    );
    assert!(!ended(&format!("{summary}Coverage: unknown (compiler emitted no coverage_summary transport row; older toolchain?)\n"), "0").is_empty());
    assert_eq!(
        ended(
            &format!(
                "{summary}Coverage: 12/12 eligible function bodies accounted for (10 verified, 2 excluded: test-harness) (complete)\n"
            ),
            "1"
        ),
        ""
    );
    assert!(
        !ended(
            &format!("{summary}Coverage: 11/12 eligible function bodies accounted for (9 verified, 2 excluded: test-harness) (complete)\n"),
            "1"
        )
        .is_empty(),
        "counts that disagree are no complete coverage, in either wording"
    );
}

#[test]
fn a_check_that_did_not_finish_is_never_scored() {
    let check = |ended: &str, budget: &str| {
        Decision::Gate(Measured {
            rc: "101".into(),
            summary: "Summary: 207 proved (2037 total)".into(),
            ended: ended.into(),
            wall_budget: budget.into(),
            ..Measured::default()
        })
    };
    let run = |d: Decision| {
        facts(vec![
            ("alpha", d),
            ("beta", gate("Summary: 3 proved (3 total)")),
        ])
    };
    let low = format!("alpha\t200\t2037\t{DRV}\nbeta\t3\t3\t{DRV}\n");
    let v = judge(
        &run(check("", "")),
        Some(&low),
        None,
        "R",
        "M",
        Update::NONE,
    );
    assert_eq!((v.word, v.exit, v.total.as_str()), ("PASS", 0, "210/2040"));
    let v = judge(
        &run(check("cargo: trustc ended by signal 15 (SIGTERM)", "")),
        Some(&low),
        None,
        "R",
        "M",
        Update::NONE,
    );
    assert_eq!(
        (v.word, v.exit, v.total.as_str()),
        ("FAIL", 1, "3/3"),
        "{}",
        v.report
    );
    assert_eq!(v.failed, ["alpha NOT-VERIFIED(incomplete)"]);
    let v = judge(
        &run(check("", "5400")),
        Some(&low),
        None,
        "R",
        "M",
        Update::NONE,
    );
    assert_eq!((v.word, v.exit, v.total.as_str()), ("FAIL", 1, "3/3"));
    assert_eq!(v.failed, ["alpha OVER-BUDGET(5400s)"]);
    let unfinished = [check("x", ""), check("", "60")];
    for d in &unfinished {
        for (ratchet, update) in [
            (format!("beta\t3\t3\t{DRV}\n"), Update::RAISE),
            (
                format!("alpha\t100\t2037\t{DRV}\nbeta\t3\t3\t{DRV}\n"),
                Update::RAISE,
            ),
            (
                format!("alpha\t274\t2037\tanother prover\nbeta\t3\t3\t{DRV}\n"),
                Update::RAISE,
            ),
            (
                format!("alpha\t274\t2037\tanother prover\nbeta\t3\t3\t{DRV}\n"),
                Update {
                    on: true,
                    reason: Some("the new prover proves less"),
                },
            ),
        ] {
            let v = judge(&run(d.clone()), Some(&ratchet), None, "R", "M", update);
            assert_eq!(v.exit, 1);
            assert!(
                v.ratchet
                    .as_deref()
                    .is_none_or(|r| !r.contains("alpha\t207")),
                "a floor from a check that did not finish: {:?}",
                v.ratchet
            );
            assert!(v.memory_rows.iter().all(|(n, _)| n != "alpha"));
            assert!(v.host_obs.iter().all(|(n, _)| n != "alpha"));
        }
    }
    let f = run(check("cargo: trustc ended by signal 15 (SIGTERM)", "5400"));
    assert_eq!(Facts::parse(&f.to_record()), Some(f.clone()));
    assert_eq!(
        Facts::parse(&f.to_record().replace(FACTS_MAGIC, "aterm-trust facts 5")),
        None,
        "a version-5 record, which could not tell, is never read"
    );
}

/// TP-4: trustd ending under a check (the lane's keeper saw its grant end) leaves trustc to
/// die of SIGPIPE writing to it with nothing measured, which says nothing of the code: COULD
/// NOT RUN naming what then served the socket, and no floor moves. NEGATIVE CONTROL: the
/// same check with no trustd lost is the code's (`NOT-VERIFIED(rc=101)`, or
/// `NOT-VERIFIED(incomplete)`), and one that finished before trustd went is scored.
#[test]
fn a_check_whose_trustd_went_away_is_could_not_run_and_never_the_codes() {
    const GONE: &str = "nothing serves /tmp/t.sock (its socket file is stale)";
    const PIPE: &str = "cargo: trustc ended by signal 13 (SIGPIPE)";
    let check = |rc: &str, lost: &str, transport: bool, ended: &str, summary: &str| {
        Decision::Gate(Measured {
            rc: rc.into(),
            transport,
            ended: ended.into(),
            summary: summary.into(),
            trustd_lost: lost.into(),
            ..Measured::default()
        })
    };
    let ratchet = format!("alpha\t3\t3\t{DRV}\nbeta\t3\t3\t{DRV}\n");
    let judged = |d: Decision| {
        let f = facts(vec![
            ("alpha", d),
            ("beta", gate("Summary: 3 proved (3 total)")),
        ]);
        assert_eq!(Facts::parse(&f.to_record()), Some(f.clone()));
        judge(&f, Some(&ratchet), None, "R", "M", Update::RAISE)
    };
    for d in [
        check("101", GONE, true, "", ""),
        check("101", GONE, false, PIPE, ""),
        check("101", GONE, false, "", ""),
    ] {
        let v = judged(d);
        assert_eq!((v.word, v.exit), ("COULD-NOT-RUN", 2), "{}", v.report);
        assert!(v.failed.is_empty(), "{:?}", v.failed);
        assert_eq!(v.unverified, ["alpha"]);
        assert_eq!(row_of(&v, "alpha"), "NOT-VERIFIED(trustd)");
        let said = note(&v, "NOT VERIFIED: alpha: trustd went away");
        assert!(said.contains(GONE) && said.contains("exited 101"), "{said}");
        assert!(
            v.tail.contains("trustd went away while they ran"),
            "{}",
            v.tail
        );
        assert!(
            v.ratchet
                .as_deref()
                .is_none_or(|r| r.contains("alpha\t3\t3\t")),
            "no floor moves: {:?}",
            v.ratchet
        );
    }

    let v = judged(check("101", "", true, "", ""));
    assert_eq!(v.failed, ["alpha NOT-VERIFIED(rc=101)"]);
    let v = judged(check("101", "", false, PIPE, ""));
    assert_eq!(v.failed, ["alpha NOT-VERIFIED(incomplete)"]);
    let v = judged(check("0", GONE, false, "", "Summary: 3 proved (3 total)"));
    assert_eq!(
        (v.word, v.exit, v.total.as_str()),
        ("PASS", 0, "6/6"),
        "{}",
        v.report
    );
    assert!(
        v.report
            .contains("TRUSTD: went away under this run's keeper while these ran: alpha")
    );
}

/// TP-4: targo-trust's own `COULD NOT RUN` with nothing measured, its exit 3 (a run that is
/// no measurement and failed for that alone, "no verdict either way"), is undecided, in its
/// words, and no floor moves. NEGATIVE CONTROL: the same log at targo's exit 2 (a setup
/// error), and a refused path at exit 3, stay the lane's FAIL; one with counts is judged by
/// them.
#[test]
fn targo_trusts_own_could_not_run_is_undecided_and_never_the_codes() {
    const DEFECT: &str = "  [UNKNOWN] [transport:missing-json] missing structured Trust JSON transport (targo-trust)\n\
                          COULD NOT RUN: verification did not run to a measurement: targo-trust found 1 transport defect(s) and reports each as a row of its own, which has no stable key, so the unit's verdicts cannot be compared, so this run has no counts\n\
                          Stop causes: unclassified=1 — COULD NOT RUN: resource admission refused the solver, or a row carried no stable key, so this run is not a measurement\n";
    const NO_COMPILER: &str = "targo trust: COULD NOT RUN: interrupted by SIGTERM. The run started no compiler, so it has no verdict\n";
    let ratchet = format!("alpha\t3\t3\t{DRV}\nbeta\t3\t3\t{DRV}\n");
    let judged = |log: &str, rc: &str| {
        let m = Measured::of_log(log.as_bytes(), rc);
        // A run with a refused path is never filed, so its record keeps none.
        let filed = m.path_refused.is_empty();
        let f = facts(vec![
            ("alpha", Decision::Gate(m)),
            ("beta", gate("Summary: 3 proved (3 total)")),
        ]);
        if filed {
            assert_eq!(Facts::parse(&f.to_record()), Some(f.clone()));
        }
        judge(&f, Some(&ratchet), None, "R", "M", Update::RAISE)
    };
    for (log, said) in [
        (
            DEFECT,
            "COULD NOT RUN: verification did not run to a measurement: targo-trust found 1",
        ),
        (NO_COMPILER, "COULD NOT RUN: interrupted by SIGTERM."),
    ] {
        let v = judged(log, "3");
        assert_eq!((v.word, v.exit), ("COULD-NOT-RUN", 2), "{}", v.report);
        assert!(v.failed.is_empty(), "{:?}", v.failed);
        assert_eq!(v.unverified, ["alpha"]);
        assert_eq!(row_of(&v, "alpha"), "NOT-VERIFIED(targo)");
        let note = note(&v, "NOT VERIFIED: alpha: targo-trust exited 3");
        assert!(note.contains(said), "{note}");
        assert!(
            v.tail
                .contains("targo-trust could not run them to a measurement"),
            "{}",
            v.tail
        );
        assert!(
            v.ratchet
                .as_deref()
                .is_none_or(|r| r.contains("alpha\t3\t3\t")),
            "no floor moves: {:?}",
            v.ratchet
        );
    }

    // NEGATIVE CONTROL: targo's exit 2 is its setup error, and a refused path keeps its
    // remedy: the lane's FAIL, as before.
    assert_eq!(judged(DEFECT, "2").failed, ["alpha NOT-VERIFIED(rc=2)"]);
    let v = judged(REFUSED_LOG, "3");
    assert_eq!(v.failed, ["alpha NOT-VERIFIED(rc=3)"]);
    assert!(!v.report.contains("NOT-VERIFIED(targo)"), "{}", v.report);
    // With counts, they decide.
    let v = judged(
        "Summary: 3 proved (3 total)\n\
         Coverage: 9/9 eligible function bodies verified (complete)\n\
         Stop causes: unclassified=1 — COULD NOT RUN: resource admission refused the solver\n",
        "3",
    );
    assert_eq!((v.word, v.exit), ("PASS", 0), "{}", v.report);
}

#[test]
fn the_lanes_budget_mark_is_read_into_the_facts() {
    let scratch = crate::test_scratch_dir("budget");
    let dir = scratch.path();
    for (spec, rc, mark) in [
        ("alpha@1.0.0", "137", Some("60\n")),
        ("beta@2.0.0", "1", Some("\n")),
        ("gamma@3.0.0", "1", None),
    ] {
        fs::write(dir.join(format!("{spec}.log")), DONE).unwrap();
        fs::write(dir.join(format!("{spec}.log.rc")), rc).unwrap();
        if let Some(mark) = mark {
            fs::write(dir.join(format!("{spec}.log.budget")), mark).unwrap();
        }
    }
    let mut p = Plan::parse(&format!(
        "{PLAN_MAGIC}\nlogdir {}\ncrate alpha\talpha@1.0.0\tgate\ncrate beta\tbeta@2.0.0\tgate\ncrate gamma\tgamma@3.0.0\tgate\n",
        dir.display()
    ))
    .unwrap();
    p.measure();
    let got: Vec<(String, bool)> = p
        .facts
        .crates
        .iter()
        .map(|c| match &c.decision {
            Decision::Gate(m) => (m.wall_budget.clone(), !m.ended.is_empty()),
            other => panic!("{other:?}"),
        })
        .collect();
    let want: Vec<(String, bool)> = vec![
        ("60".to_string(), true),
        ("?".to_string(), false),
        (String::new(), false),
    ];
    assert_eq!(got, want);
}

fn gate(summary: &str) -> Decision {
    Decision::Gate(Measured {
        rc: "0".into(),
        summary: summary.into(),
        ..Measured::default()
    })
}

fn facts(crates: Vec<(&str, Decision)>) -> Facts {
    Facts {
        driver: "/store/trust/9192/bin/targo".into(),
        driver_id: "targo 1.99.0-dev (aaaaaaaaa 2026-09-17) (targo 0.1.0)".into(),
        trustd: "/tmp/t.sock".into(),
        grants: "1".into(),
        crates: crates.into_iter().map(|(n, d)| Crate::new(n, d)).collect(),
        ..Facts::default()
    }
}

const DRV: &str = "targo 1.99.0-dev (aaaaaaaaa 2026-09-17) (targo 0.1.0)";

#[test]
fn a_record_round_trips_and_a_cut_short_one_is_none() {
    let mut f = facts(vec![
        ("alpha", gate("  Summary: 3 proved (3 total)")),
        ("beta", Decision::NoLib),
        ("forky", Decision::Queued("queued".into())),
    ]);
    f.vanished.push("ghost".into());
    f.ambiguous
        .push(("dup".into(), "dup@1".into(), "dup@2".into()));
    f.crates.push(Crate::new("dup", Decision::Ambiguous));
    f.provenance = Some(Provenance {
        key: "k".into(),
        ..Provenance::default()
    });
    let text = f.to_record();
    assert_eq!(Facts::parse(&text), Some(f.clone()));
    let cut = text.trim_end().strip_suffix(FACTS_END).unwrap();
    assert_eq!(Facts::parse(cut), None, "no `end`, no record");
    assert_eq!(
        Facts::parse(&text.replace(FACTS_MAGIC, "aterm-trust facts 0")),
        None
    );
    let schema1 = text
        .replace(FACTS_MAGIC, "aterm-trust facts 1")
        .replace("key k\n", "key k\nenv none\n");
    assert_eq!(Facts::parse(&schema1), None);
    assert_eq!(
        Facts::parse(&format!("{text}crate x\tgate\n")),
        None,
        "nothing after `end`"
    );
}

use crate::ledger::testing::Git;

struct World {
    _dir: aterm_tempfile::TempDir,
    root: PathBuf,
}

const PROVER_COMMIT: &str = "321aaeda75478038420d97830f14724c33c8dc39";

impl World {
    fn new(label: &str) -> World {
        let dir = crate::test_scratch_dir(label);
        let root = dir.path().to_path_buf();
        fs::create_dir_all(root.join("work")).unwrap();
        let w = World { _dir: dir, root };
        crate::ledger::testing::init(
            &w.work(),
            &[
                ("user.name", "Test"),
                ("user.email", "test@example.invalid"),
                ("commit.gpgsign", "false"),
            ],
        );
        w
    }
    fn work(&self) -> PathBuf {
        self.root.join("work")
    }
    fn git(&self, args: &[&str]) -> String {
        let out = crate::ledger::testing::git()
            .arg("-C")
            .arg(self.work())
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }
    fn commit(&self, files: &[(&str, &str)]) -> String {
        for (path, body) in files {
            let full = self.work().join(path);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(full, body).unwrap();
        }
        if !files.is_empty() {
            let mut add = vec!["add", "--"];
            add.extend(files.iter().map(|(path, _)| *path));
            self.git(&add);
        }
        self.git(&["commit", "-q", "--allow-empty", "-m", "c"]);
        self.git(&["rev-parse", "HEAD"])
    }
    fn cli(&self) -> Git {
        Git::new(self.work())
    }
    fn prover(&self, name: &str) -> Prover {
        testing::prover(&self.root, name, PROVER_COMMIT)
    }
    fn store(&self) -> PathBuf {
        self.work().join(".git/aterm-verify/receipts")
    }
    fn file(&self, commit: &str, prover: &Prover, proved: u64, total: u64) -> PathBuf {
        let alpha = testing::gated(
            "alpha",
            &format!("Summary: {proved} proved ({total} total)"),
        );
        testing::file(&self.cli(), &self.store(), prover, commit, DRV, vec![alpha])
    }
    fn found(&self, prover: &Prover, rev: &str) -> std::result::Result<(Facts, String), String> {
        let head = self.git(&["rev-parse", &format!("{rev}^{{commit}}")]);
        facts_for(&self.cli(), &self.store(), prover, &head).unwrap()
    }
    fn judged(&self, prover: &Prover) -> Verdict {
        let (facts, _) = self.found(prover, "HEAD").expect("the facts are found");
        let ratchet = ratchet_at(&self.cli(), "HEAD").unwrap();
        judge(
            &facts,
            ratchet.as_deref(),
            None,
            RATCHET,
            MEMORY,
            Update::NONE,
        )
    }
}

fn floor(n: u64) -> String {
    format!("alpha\t{n}\t9\t{DRV}\n")
}

#[test]
fn the_up_only_rule_holds_row_by_row() {
    let d = "targo 1.99.0-dev (aaaaaaaaa 2026-09-17) (targo 0.1.0)";
    let base = format!("alpha\t987\t3806\t{d}\nbeta\t5\t9\t{d}\nold\t3\t3\tolder prover\n");
    let all = |_: &str| true;
    let low = |tree: &str| floors_lowered(&base, tree, &all, Some(d));
    assert!(low(&base).is_empty(), "unchanged");
    assert!(
        low(&base.replace("987\t3806", "990\t3806")).is_empty(),
        "raised"
    );
    assert!(
        low(&base.replace("987\t3806", "987\t3807")).is_empty(),
        "a total raised to the one measured"
    );
    let got = low(&base.replace("987\t3806", "900\t3806"));
    assert_eq!(got.len(), 1, "{got:?}");

    let said = base.replace(
        &format!("987\t3806\t{d}\n"),
        &format!("985\t3800\t{d}\tthe code left\n"),
    );
    assert!(low(&said).is_empty(), "a reason, the total down as far");
    let excuse = said.replace("985\t3800", "900\t3800");
    assert_eq!(
        low(&excuse).len(),
        1,
        "a reason does not excuse a lowering past the total"
    );
    assert!(
        low(&base.replace("older prover", d)).is_empty(),
        "re-labelled onto the measuring prover"
    );
    let rebased = base.replace("3\t3\tolder prover", &format!("1\t3\t{d}"));
    assert_eq!(low(&rebased).len(), 1);
    assert!(
        low(&base.replace(
            "3\t3\tolder prover",
            &format!("1\t3\t{d}\tthe new seal proves two fewer")
        ))
        .is_empty(),
        "a reasoned re-base"
    );
    assert_eq!(
        low(&base.replace("3\t3\tolder prover", "1\t3\tolder prover")).len(),
        1
    );
    let relabel = low(&base.replace(&format!("5\t9\t{d}"), "5\t9\telsewhere"));
    assert_eq!(relabel.len(), 1, "{relabel:?}");
    assert_eq!(
        low(&base.replace(&format!("beta\t5\t9\t{d}\n"), "")).len(),
        1
    );
    assert!(
        floors_lowered(
            &base,
            &base.replace(&format!("beta\t5\t9\t{d}\n"), ""),
            &|n| n != "beta",
            Some(d)
        )
        .is_empty(),
        "a library the facts do not verify"
    );
    let added = format!("{base}gamma\t4\t4\tsomeone\n");
    assert_eq!(low(&added).len(), 1, "a first floor under another name");
    assert!(
        low(&format!("{base}gamma\t4\t4\t{d}\n")).is_empty(),
        "a first floor under the measuring prover"
    );
    assert!(
        floors_lowered(&base, &added, &|n| n != "gamma", Some(d)).is_empty(),
        "a first floor for a library the facts do not verify"
    );
    assert_eq!(low(&base.replace("987\t3806", "9x7\t3806")).len(), 1);
    assert_eq!(
        floors_lowered(&base, &base.replace("older prover", d), &all, None).len(),
        1
    );
}

#[test]
fn another_prover_is_another_key() {
    let w = World::new("prover");
    let (a, b) = (w.prover("a"), w.prover("b"));
    assert_ne!(a.identity, b.identity, "two builds, one trustc commit");
    let real = Prover::at(&a.bin);
    assert_eq!(
        (&real.identity, &real.trustc),
        (&a.identity, &a.trustc),
        "the fixture is what Prover::at answers"
    );
    let head = w.commit(&[("src/lib.rs", "v1"), (RATCHET, &floor(5))]);
    w.file(&head, &a, 5, 9);
    assert!(w.found(&a, "HEAD").is_ok());
    let why = w.found(&b, "HEAD").unwrap_err();
    assert!(why.contains("no whole-tree Trust facts"), "{why}");
    let bare = Prover::at(&w.root.join("nowhere/bin"));
    assert!(w.found(&bare, "HEAD").is_err());
}

#[test]
fn a_record_under_another_key_is_never_read() {
    let w = World::new("forged");
    let a = w.prover("a");
    let first = w.commit(&[("src/lib.rs", "v1"), (RATCHET, &floor(5))]);
    let filed = w.file(&first, &a, 5, 9);
    let second = w.commit(&[("src/lib.rs", "v2")]);
    let tsr = tree_sans_floors(&w.cli(), &second).unwrap();
    let key = facts_key(&tsr, a.identity.as_ref().unwrap(), LANE_WHOLE_TREE);
    let name = w.store().join(format!("{FACTS_PREFIX}{key}"));
    fs::copy(&filed, &name).unwrap();
    assert!(w.found(&a, "HEAD").is_err());
    fs::write(&name, format!("{FACTS_MAGIC}\ncrate alpha\tgate\n")).unwrap();
    assert!(w.found(&a, "HEAD").is_err());
}

#[test]
fn the_fences_still_fire_from_stored_facts() {
    let w = World::new("fences");
    let a = w.prover("a");
    let head = w.commit(&[("src/lib.rs", "v1"), (RATCHET, &floor(5))]);
    let tsr = tree_sans_floors(&w.cli(), &head).unwrap();
    let id = a.identity.clone().unwrap();
    let file_with = |d: Decision| {
        let mut f = facts(vec![("alpha", d)]);
        f.provenance = Some(Provenance {
            key: facts_key(&tsr, &id, LANE_WHOLE_TREE),
            tree_sans_floors: tsr.clone(),
            prover: id.clone(),
            lane: LANE_WHOLE_TREE.into(),
            head: w.git(&["rev-parse", "HEAD"]),
            ..Provenance::default()
        });
        file_record(&w.store(), &f).unwrap();
    };
    file_with(gate("Summary: 5 proved (9 total)"));
    assert_eq!(w.judged(&a).word, "PASS", "(control)");
    let cases: [(&str, Decision, &str); 4] = [
        (
            &format!("alpha\t5\t8\t{DRV}\tone proved obligation left with its code\n"),
            gate("Summary: 5 proved (9 total)"),
            "TOTAL-ABOVE-ROW(>8)",
        ),
        (
            "beta\t1\t1\n",
            gate("Summary: 5 proved (9 total)"),
            "alpha NO-FLOOR",
        ),
        (
            &floor(5),
            Decision::Gate(Measured {
                rc: "2".into(),
                transport: true,
                ..Measured::default()
            }),
            "NOT-VERIFIED(rc=2)",
        ),
        (&floor(5), gate(""), "GATE-BROKE(rc=0)"),
    ];
    for (ratchet, decision, needle) in cases {
        w.commit(&[(RATCHET, ratchet)]);
        file_with(decision);
        let v = w.judged(&a);
        assert!(
            v.word == "FAIL" && v.failed.iter().any(|row| row.contains(needle)),
            "{needle}: {:?}",
            v.failed
        );
    }
    w.commit(&[(RATCHET, &floor(5))]);
    file_with(Decision::Gate(Measured {
        rc: "0".into(),
        summary: "Summary: 4 proved (9 total)".into(),
        refused: 1,
        refused_why: "trustd unreachable: 1".into(),
        ..Measured::default()
    }));
    assert_eq!(w.judged(&a).word, "COULD-NOT-RUN");
}

#[test]
fn conclude_files_only_what_a_release_may_read() {
    let w = World::new("conclude");
    let a = w.prover("a");
    let head = w.commit(&[("src/lib.rs", "v1"), (RATCHET, &floor(3))]);
    let logs = w.root.join("logs");
    let slots = w.root.join("troot");
    fs::create_dir_all(slots.join("j0/debug/deps")).unwrap();
    fs::create_dir_all(&logs).unwrap();
    fs::write(
        logs.join("alpha@1.0.0.log"),
        "  Summary: 3 proved, 0 failed (3 total)\nCoverage: 2/2 eligible function bodies verified (complete)\n",
    )
    .unwrap();
    fs::write(logs.join("alpha@1.0.0.log.rc"), "0\n").unwrap();
    fs::write(logs.join(".trustd-grants"), "1\n").unwrap();
    let dep = slots.join("j0/debug/deps/alpha-1.d");
    let plain = "alpha.rmeta: crates/alpha/src/lib.rs\n";
    fs::write(&dep, plain).unwrap();
    const LANE: &str = "lane-pin SOURCE_DATE_EPOCH=0\nlane-pin ATERM_BUILD_GIT_COMMIT=trust-lane\n\
                        lane-passed PATH\nlane-passed HOME\nlane-passed LANG\nlane-passed CARGO_TARGET_DIR\n";
    let plan = |extra: &str| {
        format!(
            "{PLAN_MAGIC}\ndriver {}/targo\ndriver-id {DRV}\ntrustd /tmp/t.sock\nonly \n\
             logdir {}\nratchet {}\ncrate alpha\talpha@1.0.0\tgate\n\
             store {}\ncommit {head}\ntree x\nprover {}\nprover-bin {}\ntarget-root {}\n{LANE}{extra}",
            a.bin.display(),
            logs.display(),
            w.work().join(RATCHET).display(),
            w.store().display(),
            a.identity.as_ref().unwrap(),
            a.bin.display(),
            slots.display(),
        )
    };
    let run = |extra: &str| {
        let mut p = Plan::parse(&plan(extra)).unwrap();
        conclude(&w.cli(), &w.work(), &mut p, Update::NONE).unwrap()
    };
    let facts_line = |extra: &str| run(extra).facts_line.unwrap_or_default();
    assert!(facts_line("file the checkout is not clean\n").starts_with("FACTS: none"));
    assert!(
        run("").facts_line.is_none(),
        "a run that does not file says nothing"
    );
    let mut bare = Plan::parse(&plan("file \n").replace(LANE, "")).unwrap();
    let l = conclude(&w.cli(), &w.work(), &mut bare, Update::NONE)
        .unwrap()
        .facts_line
        .unwrap();
    assert!(l.starts_with("FACTS: none"), "{l}");
    fs::write(
        &dep,
        format!("alpha.rmeta: crates/alpha/src/lib.rs {RATCHET}\n"),
    )
    .unwrap();
    assert!(facts_line("file \n").starts_with("FACTS: none"));
    fs::write(
        &dep,
        format!("alpha.rmeta: crates/alpha/src/lib.rs {MEMORY}\n"),
    )
    .unwrap();
    assert!(facts_line("file \n").starts_with("FACTS: none"));
    let reads = |lines: &str| {
        fs::write(&dep, format!("{plain}\n{lines}")).unwrap();
        facts_line("file \n")
    };
    for lines in [
        "# env-dep:ATERM_RELEASE_BUILD=1\n",
        "# env-dep:HOME\n",
        "# env-dep:LANG=en_US.UTF-8\n# env-dep:HOME=/Users//x\n",
        "# env-dep:SOURCE_DATE_EPOCH=5\n",
        "# env-dep:ATERM_GIT_COMMIT=trust-lane\n",
    ] {
        let l = reads(lines);
        assert!(
            l.starts_with("FACTS: none") && !l.contains("/Users//x"),
            "{lines}: {l}"
        );
    }
    let build = slots.join("j0/debug/build/aterm-gui-0123/output");
    fs::create_dir_all(build.parent().unwrap()).unwrap();
    fs::write(
        &build,
        "cargo:rerun-if-env-changed=ATERM_BUILD_GIT_COMMIT\ncargo:rustc-env=ATERM_GIT_COMMIT=trust-lane\n",
    )
    .unwrap();
    let l = reads(
        "# env-dep:ATERM_RELEASE_BUILD\n# env-dep:CARGO_PKG_VERSION=1.0.0\n# env-dep:OUT_DIR=/x/out\n\
         # env-dep:SOURCE_DATE_EPOCH=0\n# env-dep:ATERM_GIT_COMMIT=trust-lane\n",
    );
    assert!(l.starts_with("FACTS: ") && !l.contains("none"), "{l}");
    fs::remove_file(&build).unwrap();
    fs::write(&dep, plain).unwrap();
    let home = w.root.join("cargo-home");
    fs::create_dir_all(&home).unwrap();
    fs::write(
        home.join("config.toml"),
        "[profile.dev]\ndebug-assertions = false\n",
    )
    .unwrap();
    let l = facts_line(&format!("file \ncargo-home {}\n", home.display()));
    assert!(l.starts_with("FACTS: none"), "{l}");
    let l = run("file \n").facts_line.unwrap();
    assert!(
        l.starts_with("FACTS: ") && !l.contains("none"),
        "the control files: {l}"
    );
    assert!(w.found(&a, "HEAD").is_ok(), "the control is found");
    fs::remove_file(&dep).unwrap();
    let l = facts_line("file \n");
    assert!(
        l.starts_with("FACTS: none") && l.contains("no dep-info"),
        "{l}"
    );
    fs::write(&dep, plain).unwrap();
    let mut unlisted =
        Plan::parse(&plan("file \n").replace(&format!("commit {head}"), "commit 0000")).unwrap();
    let done = conclude(&w.cli(), &w.work(), &mut unlisted, Update::NONE).unwrap();
    let l = done.facts_line.unwrap();
    assert!(
        l.starts_with("FACTS: none") && l.contains("git could not list"),
        "{l}"
    );
    assert_eq!(done.verdict.word, "PASS");
    let mut moved =
        Plan::parse(&plan("file \n").replace(a.identity.as_ref().unwrap(), "0000")).unwrap();
    let l = conclude(&w.cli(), &w.work(), &mut moved, Update::NONE)
        .unwrap()
        .facts_line
        .unwrap();
    assert!(l.starts_with("FACTS: none"), "{l}");
    let done = run("");
    assert_eq!(done.verdict.word, "PASS");
}

#[test]
fn the_trust_verbs_answer_three_ways() {
    let w = World::new("cli");
    let a = w.prover("a");
    let measured = format!("alpha\t6\t10\t{DRV}\n");
    let head = w.commit(&[("src/lib.rs", "v1"), (RATCHET, &measured)]);
    let judge_at = |commit: &str, update| TrustCmd::Judge {
        commit: commit.into(),
        prover: Some(a.bin.clone()),
        update,
        reason: None,
    };
    let judge_cmd = |update| judge_at("HEAD", update);
    assert_eq!(
        run_cmd(&w.work(), judge_cmd(false)),
        1,
        "no facts: not verified"
    );
    assert_eq!(
        run_cmd(&w.work(), judge_at("no-such-rev", false)),
        2,
        "no commit: no answer"
    );
    w.file(&head, &a, 5, 9);
    let working = || fs::read_to_string(w.work().join(RATCHET)).unwrap();
    assert_eq!(run_cmd(&w.work(), judge_cmd(false)), 1, "5 of a floor of 6");
    fs::write(w.work().join(RATCHET), floor(5)).unwrap();
    assert_eq!(run_cmd(&w.work(), judge_cmd(false)), 1);
    assert_eq!(working(), floor(5));
    let said = format!("alpha\t5\t9\t{DRV}\tone proved obligation left with its code\n");
    fs::write(w.work().join(RATCHET), &said).unwrap();
    assert_eq!(run_cmd(&w.work(), judge_cmd(false)), 0);
    fs::write(w.work().join(RATCHET), &measured).unwrap();
    assert_eq!(run_cmd(&w.work(), judge_cmd(true)), 1);
    assert_eq!(working(), measured);
    let w = World::new("cli-first");
    let a = w.prover("a");
    let head = w.commit(&[("src/lib.rs", "v1"), (RATCHET, "")]);
    w.file(&head, &a, 5, 9);
    let judge_cmd = |update| TrustCmd::Judge {
        commit: "HEAD".into(),
        prover: Some(a.bin.clone()),
        update,
        reason: None,
    };
    assert_eq!(run_cmd(&w.work(), judge_cmd(false)), 1, "NO-FLOOR");
    assert_eq!(run_cmd(&w.work(), judge_cmd(true)), 0);
    assert_eq!(
        fs::read_to_string(w.work().join(RATCHET)).unwrap(),
        format!("alpha\t5\t9\t{DRV}\n")
    );
}

#[test]
fn the_key_leaves_out_the_floors_and_nothing_else() {
    let w = World::new("key");
    let one = w.commit(&[
        ("src/lib.rs", "v1"),
        (RATCHET, "alpha\t1\t1\n"),
        (MEMORY, "alpha\t16.0\t1.4\t=\t-\n"),
    ]);
    let two = w.commit(&[(RATCHET, "alpha\t2\t2\n")]);
    let mem = w.commit(&[(MEMORY, "alpha\t16.0\t1.6\t=\t2026-09-30 at 16.0 GiB\n")]);
    let costs = w.commit(&[(COSTS, "alpha\t1\t1\t7\t2026-10-16T00:00:00Z\n")]);
    let stops = w.commit(&[(BACKSTOPS, "alpha\t0123456789abcdef\twall-backstop\tf\n")]);
    let three = w.commit(&[("crates/alpha/src/lib.rs", "pub fn f() {}\n")]);
    let t = |c: &str| tree_sans_floors(&w.cli(), c).unwrap();
    assert_eq!(t(&one), t(&two), "the floors alone move nothing");
    assert_eq!(t(&two), t(&mem), "nor does the memory record");
    assert_eq!(t(&mem), t(&costs), "nor does the cost-history seed");
    assert_eq!(t(&costs), t(&stops), "nor do the floors' backstops");
    assert_ne!(t(&mem), t(&three), "any other file is an input");
    w.git(&["rm", "-q", RATCHET, MEMORY, COSTS, BACKSTOPS]);
    let four = w.commit(&[]);
    assert_eq!(t(&three), t(&four));
    let k = |t: &str, p: &str, l: &str| facts_key(t, p, l);
    assert_ne!(k("t", "p1", LANE_WHOLE_TREE), k("t", "p2", LANE_WHOLE_TREE));
    assert_ne!(k("t", "p", LANE_WHOLE_TREE), k("u", "p", LANE_WHOLE_TREE));
    assert_ne!(k("t", "p", LANE_WHOLE_TREE), k("t", "p", "narrowed"));
}

#[test]
fn the_judge_holds_floors_as_the_lane_did() {
    let f = facts(vec![("alpha", gate("Summary: 3 proved (3 total)"))]);
    let pass = judge(
        &f,
        Some(&format!("alpha\t3\t3\t{DRV}\n")),
        None,
        "R",
        "M",
        Update::NONE,
    );
    assert_eq!((pass.word, pass.exit), ("PASS", 0));
    let fail = judge(
        &f,
        Some(&format!("alpha\t4\t3\t{DRV}\n")),
        None,
        "R",
        "M",
        Update::NONE,
    );
    assert_eq!((fail.word, fail.exit), ("FAIL", 1));
    assert_eq!(fail.failed, ["alpha RATCHET-REGRESSION(<4) 3/3"]);
    let none = judge(&f, Some(""), None, "R", "M", Update::NONE);
    assert_eq!(none.word, "FAIL");
    let rec = judge(&f, None, None, "R", "M", Update::RAISE);
    assert_eq!(rec.word, "PASS");
    assert_eq!(
        rec.ratchet.as_deref(),
        Some(format!("alpha\t3\t3\t{DRV}\n").as_str())
    );
    let bad = judge(&f, Some("alpha\tthree\t3\n"), None, "R", "M", Update::NONE);
    assert_eq!(bad.word, "FAIL");
    let theirs = "targo 1.99.0-dev (bbbbbbbbb 2026-09-17) (targo 0.1.0)";
    let other = format!("alpha\t9\t9\t{theirs}\n");
    let moved = judge(&f, Some(&other), None, "R", "M", Update::NONE);
    assert_eq!((moved.word, moved.exit), ("FAIL", 1));
    assert_eq!(moved.failed, ["alpha RATCHET-REGRESSION(<9) 3/3"]);
    let kept = judge(&f, Some(&other), None, "R", "M", Update::RAISE);
    assert_eq!(kept.ratchet, None);
    let reason = "trust 5ec1 proves fewer; owner-approved";
    let lowered = judge(
        &f,
        Some(&other),
        None,
        "R",
        "M",
        Update {
            on: true,
            reason: Some(reason),
        },
    );
    assert_eq!(
        lowered.ratchet.as_deref(),
        Some(format!("alpha\t3\t3\t{DRV}\t{reason}\n").as_str())
    );
    let met = format!("alpha\t2\t2\t{theirs}\n");
    assert_eq!(
        judge(&f, Some(&met), None, "R", "M", Update::NONE).word,
        "PASS"
    );
    assert_eq!(
        judge(&f, Some(&met), None, "R", "M", Update::RAISE)
            .ratchet
            .as_deref(),
        Some(format!("alpha\t3\t3\t{DRV}\n").as_str())
    );
    for update in [
        Update::RAISE,
        Update {
            on: true,
            reason: Some(reason),
        },
    ] {
        assert_eq!(
            judge(
                &f,
                Some(&format!("alpha\t4\t3\t{DRV}\n")),
                None,
                "R",
                "M",
                update
            )
            .ratchet,
            None
        );
    }
    assert_eq!(
        judge(
            &f,
            Some(&format!("alpha\t3\t4\t{DRV}\n")),
            None,
            "R",
            "M",
            Update::RAISE
        )
        .ratchet
        .as_deref(),
        Some(format!("alpha\t3\t3\t{DRV}\n").as_str())
    );
    let why = judge(
        &f,
        Some(&format!("alpha\t2\t2\t{DRV}\twent with its code\n")),
        None,
        "R",
        "M",
        Update::NONE,
    );
    assert_eq!(why.word, "FAIL");
    assert!(why.report.contains("TOTAL-ABOVE-ROW(>2)"), "{}", why.report);
}

/// `trust judge --reason`, as `trust run`'s, needs `--update-ratchet` and words: a blank one
/// would leave the lowered row's fifth column blank.
#[test]
fn the_judges_reason_needs_words_and_update_ratchet() {
    let reason = |args: &[&str]| {
        let mut it = std::iter::once("judge").chain(args.iter().copied());
        parse_cmd(&mut it).map(|c| match c {
            TrustCmd::Judge { reason, .. } => reason,
            other => panic!("{other:?}"),
        })
    };
    assert_eq!(
        reason(&["--update-ratchet", "--reason", "re-based"]),
        Ok(Some("re-based".into()))
    );
    assert_eq!(reason(&["--update-ratchet"]), Ok(None));
    for args in [
        &["--reason", "re-based"][..],
        &["--update-ratchet", "--reason", ""],
        &["--update-ratchet", "--reason", " "],
        &["--update-ratchet", "--reason", "\t\r\n"],
    ] {
        assert!(reason(args).is_err(), "{args:?}");
    }
}

/// A `--reason` that lowered no floor is said, with what became of each library it was given
/// for, never dropped without a word; one a row keeps, trimmed, is not, nor is a run with none.
#[test]
fn a_reason_that_lowers_nothing_is_said() {
    let theirs = "targo 1.99.0-dev (bbbbbbbbb 2026-09-17) (targo 0.1.0)";
    let f = facts(vec![
        ("alpha", gate("Summary: 3 proved (3 total)")),
        ("beta", gate("Summary: 2 proved (4 total)")),
        ("gamma", Decision::NoLib),
    ]);
    let why = "the new prover asks other obligations";
    let reasoned = Update {
        on: true,
        reason: Some(why),
    };
    let unused = |v: &Verdict| {
        v.report
            .lines()
            .find(|l| l.starts_with("REASON UNUSED"))
            .map(str::to_string)
    };
    let met = judge(
        &f,
        Some(&format!("alpha\t2\t3\t{theirs}\nbeta\t3\t4\t{DRV}\n")),
        None,
        "R",
        "M",
        reasoned,
    );
    assert_eq!(
        met.ratchet.as_deref(),
        Some(format!("alpha\t3\t3\t{DRV}\nbeta\t3\t4\t{DRV}\n").as_str()),
        "alpha re-based with no reason, beta's own floor kept"
    );
    assert_eq!(
        unused(&met).as_deref(),
        Some(
            "REASON UNUSED: --reason lowered no floor, so no row keeps it: alpha: 3 proved, \
             meeting its floor of 2; beta: 2 proved, below its floor of 3, which this prover \
             recorded: a reason lowers only another prover's floor; gamma: NO-LIB, nothing \
             to lower"
        ),
        "{}",
        met.report
    );
    let floors = format!("alpha\t9\t9\t{theirs}\nbeta\t2\t4\t{DRV}\n");
    let lowered = judge(
        &f,
        Some(&floors),
        None,
        "R",
        "M",
        Update {
            on: true,
            reason: Some(" the new prover asks other obligations\n"),
        },
    );
    assert_eq!(
        lowered.ratchet.as_deref(),
        Some(format!("alpha\t3\t3\t{DRV}\t{why}\nbeta\t2\t4\t{DRV}\n").as_str())
    );
    assert_eq!(unused(&lowered), None, "{}", lowered.report);
    let none = judge(&f, Some(&floors), None, "R", "M", Update::RAISE);
    assert_eq!(none.ratchet.as_deref(), Some(floors.as_str()));
    assert_eq!(unused(&none), None, "{}", none.report);
}

#[test]
fn a_reasoned_rebase_is_held_to_the_memory_guard() {
    let theirs = "targo 1.99.0-dev (bbbbbbbbb 2026-09-17) (targo 0.1.0)";
    let floor = format!("alpha\t3\t3\t{theirs}\n");
    let reason = Update {
        on: true,
        reason: Some("the new seal proves two fewer"),
    };
    let run = |budget_rows: u64, peak: &str| {
        let mut f = facts(vec![(
            "alpha",
            Decision::Gate(Measured {
                rc: "0".into(),
                summary: "Summary: 1 proved (1 total)".into(),
                budget_rows,
                peak: peak.into(),
                ceiling: "1.0 GiB".into(),
                ..Measured::default()
            }),
        )]);
        f.crates[0].memory = MemRow {
            f: "16.0".into(),
            ..MemRow::default()
        };
        f.host = Host {
            mem: 8 * 1_073_741_824,
            ..Host::default()
        };
        f.measured_at = "2026-09-30T00:00:00Z".into();
        judge(&f, Some(&floor), None, "R", "M", reason)
    };
    let ran_out = run(2, "1.3 GiB");
    assert_eq!(ran_out.ratchet, None);
    let roomy = run(0, "0.6 GiB");
    assert_eq!(
        roomy.ratchet.as_deref(),
        Some(format!("alpha\t1\t1\t{DRV}\tthe new seal proves two fewer\n").as_str())
    );
    assert_eq!(
        roomy.memory_rows,
        [(
            "alpha".to_string(),
            MemRow {
                f: "1.0".into(),
                ram: "8".into(),
                at: "1.0".into(),
                need: "0.6".into(),
                bound: "=".into(),
                when: "2026-09-30".into(),
            }
        )]
    );
}

/// A prover whose trustc takes no ceiling names none in its log and ran out of none, so a
/// reasoned re-base onto it is not withheld for the ceiling its floor was set at, and the
/// floor's ceiling becomes `-`; a capped prover's log that names none still withholds it.
#[test]
fn an_uncapped_provers_run_rebases_a_floor_set_at_a_ceiling() {
    let theirs = "targo 1.99.0-dev (bbbbbbbbb 2026-09-17) (targo 0.1.0)";
    let why = "the new prover asks other obligations";
    let run = |uncapped: bool| {
        let mut f = facts(vec![("alpha", gate("Summary: 3 proved (3 total)"))]);
        f.crates[0].memory = MemRow {
            f: "6.0".into(),
            ..MemRow::default()
        };
        f.host = Host {
            uncapped,
            ..host(48)
        };
        let update = Update {
            on: true,
            reason: Some(why),
        };
        let floor = format!("alpha\t9\t9\t{theirs}\n");
        judge(&f, Some(&floor), None, "R", "M", update)
    };
    let rebased = run(true);
    assert_eq!(
        rebased.ratchet.as_deref(),
        Some(format!("alpha\t3\t3\t{DRV}\t{why}\n").as_str())
    );
    assert_eq!(
        rebased.memory_rows,
        [(
            "alpha".to_string(),
            MemRow {
                f: "-".into(),
                ..MemRow::default()
            }
        )]
    );
    let capped = run(false);
    assert_eq!(capped.ratchet, None);
    assert!(
        capped.report.contains("RE-BASE WITHHELD: alpha"),
        "{}",
        capped.report
    );
}

#[test]
fn a_log_that_names_no_ceiling_records_none() {
    let theirs = "targo 1.99.0-dev (bbbbbbbbb 2026-09-17) (targo 0.1.0)";
    let run = |skipped: Option<u64>, floor: &str, update: Update<'_>| {
        let mut f = facts(vec![(
            "alpha",
            Decision::Gate(Measured {
                rc: "0".into(),
                summary: "Summary: 3 proved (3 total)".into(),
                skipped,
                ..Measured::default()
            }),
        )]);
        f.crates[0].memory = MemRow {
            f: "1.0".into(),
            ..MemRow::default()
        };
        f.host = host(24);
        judge(&f, Some(floor), None, "R", "M", update)
    };
    let met = run(None, &format!("alpha\t2\t2\t{theirs}\n"), Update::RAISE);
    assert_eq!(
        met.ratchet.as_deref(),
        Some(format!("alpha\t3\t3\t{DRV}\n").as_str())
    );
    assert_eq!(met.memory_rows, [("alpha".to_string(), MemRow::default())]);
    let low = run(
        None,
        &format!("alpha\t9\t9\t{theirs}\n"),
        Update {
            on: true,
            reason: Some("the new seal proves fewer"),
        },
    );
    assert_eq!(low.ratchet, None);
    assert!(
        low.report
            .contains("RE-BASE WITHHELD: alpha: this run's log names no verifier ceiling"),
        "{}",
        low.report
    );
    let held = run(Some(2), &format!("alpha\t3\t3\t{DRV}\n"), Update::NONE);
    assert_eq!(
        held.host_obs,
        [(
            "alpha".to_string(),
            Some(["held", "-", "-", "3", "3"].map(String::from))
        )]
    );
}

#[test]
fn a_lapsed_shortfall_is_undecided_never_a_pass() {
    let lapsed = |proved: u64, n: u64| {
        let mut f = facts(vec![(
            "alpha",
            Decision::Gate(Measured {
                rc: "0".into(),
                summary: format!("Summary: {proved} proved (9 total)"),
                lapsed: n,
                pace: Some(STARVED),
                ..Measured::default()
            }),
        )]);
        f.provenance = Some(Provenance::default());
        f
    };
    let floor3 = format!("alpha\t3\t9\t{DRV}\n");
    let quiet = |f: Facts| {
        let mut f = f;
        if let Decision::Gate(m) = &mut f.crates[0].decision {
            m.pace = Some(QUIET);
        }
        f
    };
    let v = judge(
        &quiet(lapsed(2, 1)),
        Some(&floor3),
        None,
        "R",
        "M",
        Update::NONE,
    );
    assert_eq!(
        v.failed,
        ["alpha RATCHET-REGRESSION(<3) 2/9"],
        "a lapse in a run that was not starved is not the clock's: {}",
        v.report
    );
    let v = judge(&lapsed(2, 1), Some(&floor3), None, "R", "M", Update::NONE);
    assert_eq!((v.word, v.exit), ("COULD-NOT-RUN", 2));
    assert_eq!(v.unverified, ["alpha"]);
    assert_eq!(v.total, "0/0", "in neither total");
    assert_eq!(
        judge(&lapsed(2, 1), Some(&floor3), None, "R", "M", Update::RAISE).ratchet,
        None
    );
    let v = judge(&lapsed(1, 1), Some(&floor3), None, "R", "M", Update::NONE);
    assert_eq!(v.failed, ["alpha RATCHET-REGRESSION(<3) 1/9"]);
    let v = judge(&lapsed(3, 5), Some(&floor3), None, "R", "M", Update::NONE);
    assert_eq!(v.word, "PASS");
}

/// A proof check the lane cancelled at its deadline is the machine's in a starved run, as a
/// backstop's stop is: every one when the floor's run is unknown, measured or remeasured,
/// filed or not, and only those beyond the floor's run's count when it was kept. A run that
/// was not starved decides them.
#[test]
fn a_cancelled_proof_check_is_credited_only_in_a_starved_run() {
    let cancelled = " ERROR ay_dpll::executor::proof::check::internal internal proof checker rejected UNSAT proof, error=proof validation cancelled by the caller (interrupt, deadline, or memory limit), checked_steps=1139, skipped_hole_steps=0, total_steps=1139\n";
    let invalid = " ERROR ay_dpll::executor::proof::check::internal internal proof checker rejected UNSAT proof, error=step t5 has invalid resolution derivation, checked_steps=4, skipped_hole_steps=0, total_steps=9\n";
    let run_at = |pace: Pace, proved: u64, n: usize| {
        let text = format!(
            "{}{}    = note: native verifier status: Inconclusive; requested=82, proved=27, failed=0, unknown=34, timed_out=0, unsupported=21\n  Summary: {proved} proved, 26 failed (553 total)\nCoverage: 101/101 eligible function bodies verified (complete)\n",
            cancelled.repeat(n),
            invalid.repeat(7),
        );
        let mut m = log(&text);
        m.rc = "1".into();
        m.pace = Some(pace);
        let mut f = facts(vec![("aterm-codec", Decision::Gate(m))]);
        f.provenance = Some(Provenance::default());
        f
    };
    let run = |proved: u64, n: usize| run_at(STARVED, proved, n);
    let only = |f: &Facts| match &f.crates[0].decision {
        Decision::Gate(m) => (m.lapsed, m.cancelled),
        _ => unreachable!(),
    };
    assert_eq!(
        only(&run(310, 131)),
        (0, 131),
        "each cancellation, and no invalid derivation"
    );
    let floor = format!("aterm-codec\t383\t551\t{DRV}\n");
    let judged = |f: &Facts, backstops: Option<&str>| {
        judge(f, Some(&floor), backstops, "R", "M", Update::NONE)
    };
    let regression = ["aterm-codec RATCHET-REGRESSION(<383) 310/553"];
    let v = judged(&run(310, 131), None);
    assert_eq!(
        row_of(&v, "aterm-codec"),
        "NOT-VERIFIED(clock,<383,131)",
        "{}",
        v.report
    );
    // What is credited and the 7 the prover's spread takes from 383 must cover the 73.
    assert_eq!(
        row_of(&judged(&run(310, 66), None), "aterm-codec"),
        "NOT-VERIFIED(clock,<383,66)"
    );
    assert_eq!(judged(&run(310, 65), None).failed, regression);
    assert_eq!(judged(&run(310, 0), None).failed, regression);
    let v = judged(&run(383, 91), None);
    assert_ne!(v.word, "COULD-NOT-RUN", "{}", v.report);
    assert!(v.failed.is_empty(), "{:?}", v.failed);
    let v = judged(&run_at(QUIET, 310, 131), None);
    assert_eq!(v.failed, regression, "not starved: {}", v.report);

    let mut again = run(310, 131);
    if let Some(p) = again.provenance.as_mut() {
        p.remeasured
            .push("2026-10-01T00:00:00Z abc aterm-codec".into());
    }
    let mut unfiled = run(310, 131);
    unfiled.provenance = None;
    for f in [&again, &unfiled] {
        assert_eq!(
            judged(f, None).word,
            "COULD-NOT-RUN",
            "a starved run decides nothing its cancellations cover"
        );
    }

    let v = judged(&run(310, 131), Some(&kept_run("aterm-codec", &[], 100)));
    assert_eq!(v.failed, regression, "31 beyond the floor's run's 100");
    let note = v.report.lines().find(|l| l.starts_with("LOST: ")).unwrap();
    assert!(note.contains("31 proof check(s)"), "{note}");
    let v = judged(&run(310, 131), Some(&kept_run("aterm-codec", &[], 58)));
    assert_eq!(row_of(&v, "aterm-codec"), "NOT-VERIFIED(clock,<383,73)");
}

/// A starved run: CPU/wall 0.40 at load 30 on 14 cores.
const STARVED: Pace = Pace {
    cpu_ms: 737_200,
    wall_ms: 1_843_000,
    load: Some(3000),
    cores: 14,
    threads: None,
};

/// A quiet one: aterm-gpu's 1835 s of CPU in 1843 s at load 2 on 14 cores.
const QUIET: Pace = Pace {
    cpu_ms: 1_835_000,
    wall_ms: 1_843_000,
    load: Some(200),
    cores: 14,
    threads: None,
};

/// A real check's log and obligation record, trimmed: the report's Summary, Stop causes,
/// Coverage and Gate lines with each function's `timed_out=` note, and the rows of its
/// version 1 record for the functions a backstop stopped (aterm-render's: four of them),
/// each with the stop its line in the log names (its `[stop:…]` tag, or wall-backstop for a
/// timeout), since the report the reducer read was removed once its record was written.
const GPU: (&str, &str, &str) = (
    "aterm-gpu@0.110.0",
    include_str!("../tests/fixtures/trust/aterm-gpu@0.110.0.log"),
    include_str!("../tests/fixtures/trust/aterm-gpu@0.110.0.log.oblig.tsv"),
);
const RENDER: (&str, &str, &str) = (
    "aterm-render@0.110.0",
    include_str!("../tests/fixtures/trust/aterm-render@0.110.0.log"),
    include_str!("../tests/fixtures/trust/aterm-render@0.110.0.log.oblig.tsv"),
);

/// Measures `check` as the lane leaves it, its log beside its `.rc`, its `.pace` (when
/// given) and its obligation record (when `record`), through [`Plan::measure`].
fn measured_check(
    name: &str,
    (spec, log, oblig): (&str, &str, &str),
    pace: Option<Pace>,
    record: bool,
) -> Facts {
    let scratch = crate::test_scratch_dir("check");
    let dir = scratch.path();
    let beside = |ext: &str| dir.join(format!("{spec}.log{ext}"));
    fs::write(beside(""), log).unwrap();
    fs::write(beside(".rc"), "1\n").unwrap();
    if let Some(p) = pace {
        fs::write(beside(".pace"), format!("{}\n", p.text())).unwrap();
    }
    if record {
        fs::write(beside(crate::trust_oblig::RECORD_SUFFIX), oblig).unwrap();
    }
    let mut p = Plan::parse(&format!(
        "{PLAN_MAGIC}\nlogdir {}\ncrate {name}\t{spec}\tgate\n",
        dir.display()
    ))
    .unwrap();
    p.measure();
    p.facts.driver = "/store/trust/15698/bin/targo".into();
    p.facts.driver_id = DRV.into();
    p.facts
}

fn gated(f: &Facts) -> &Measured {
    match &f.crates[0].decision {
        Decision::Gate(m) => m,
        other => panic!("{other:?}"),
    }
}

/// The text of [`BACKSTOPS`] keeping `name`'s floor's run: what a backstop stopped in it,
/// and how many proof checks the lane cancelled.
fn kept_run(name: &str, stops: &[Backstop], cancelled: u64) -> String {
    backstops_rewritten(
        None,
        name,
        &FloorRun {
            stops: stops.to_vec(),
            cancelled,
        },
    )
}

fn row_of(v: &Verdict, name: &str) -> String {
    v.report
        .lines()
        .find(|l| l.split_whitespace().next() == Some(name))
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or_default()
        .to_string()
}

fn note<'a>(v: &'a Verdict, head: &str) -> &'a str {
    v.report
        .lines()
        .find(|l| l.starts_with(head))
        .unwrap_or_else(|| panic!("no {head} note: {}", v.report))
}

#[test]
fn a_real_checks_stops_are_read_from_its_log_and_its_record() {
    let f = measured_check("aterm-gpu", GPU, Some(QUIET), true);
    let m = gated(&f);
    assert_eq!(parse_summary(&m.summary), (Some(812), Some(4685)));
    assert_eq!((m.lapsed, m.rss, m.cancelled), (89, 0, 0));
    assert!(m.ended.is_empty(), "{}", m.ended);
    assert_eq!(m.pace, Some(QUIET));
    let stops = m.backstops.as_deref().unwrap();
    assert_eq!(stops.len(), 89, "the Stop causes line's wall-backstop=89");
    assert!(stops.iter().all(|b| b.stop == "wall-backstop"));
    assert_eq!(Facts::parse(&f.to_record()), Some(f.clone()));
    let f = measured_check("aterm-render", RENDER, None, true);
    let m = gated(&f);
    assert_eq!(parse_summary(&m.summary), (Some(1730), Some(20856)));
    assert_eq!((m.lapsed, m.rss), (747, 715), "its Stop causes line");
    let stops = m.backstops.as_deref().unwrap();
    let rss = stops.iter().filter(|b| b.stop == "rss-backstop").count();
    assert_eq!((rss, stops.len() - rss), (44, 6));
    assert_eq!(m.pace, None);
    let f = measured_check("aterm-gpu", GPU, Some(QUIET), false);
    assert_eq!(gated(&f).backstops, None, "no record, no names");
}

/// The acceptance of docs/DESIGN-adaptive-build-2026-10-24.md's TRUST-JUDGE on aterm-gpu's
/// 812 of 4685: against a floor of 813, with its floor's run having stopped all but one
/// obligation this run stopped, the lost one is named and the drop is the code's; a starved
/// run of the same is undecided, and raises nothing.
#[test]
fn a_shortfall_is_the_clocks_only_in_a_starved_run_and_only_for_a_new_stop() {
    let floor = format!("aterm-gpu\t813\t4685\t{DRV}\n");
    let rec = crate::trust_oblig::Record::parse(GPU.2).unwrap();
    let alike =
        |o: &crate::trust_oblig::Obligation| rec.rows.iter().filter(|r| r.key() == o.key()).count();
    let lost = rec
        .rows
        .iter()
        .find(|o| o.backstopped() && o.status == "timeout" && alike(o) == 1)
        .unwrap();
    let all = gated(&measured_check("aterm-gpu", GPU, None, true))
        .backstops
        .clone()
        .unwrap();
    let mut had = all.clone();
    let at = had.iter().position(|b| b.key == lost.key()).unwrap();
    had.remove(at);
    let had = kept_run("aterm-gpu", &had, 0);
    let judged = |pace: Pace, backstops: Option<&str>, update: Update<'_>| {
        let f = measured_check("aterm-gpu", GPU, Some(pace), true);
        judge(&f, Some(&floor), backstops, "R", "M", update)
    };

    let v = judged(QUIET, Some(&had), Update::NONE);
    assert_eq!(v.failed, ["aterm-gpu RATCHET-REGRESSION(<813) 812/4685"]);
    let lost_note = note(&v, "LOST: ");
    assert!(
        lost_note.contains(&lost.said()) && lost_note.contains(&QUIET.said()),
        "{lost_note}"
    );
    assert!(!v.report.contains("BUDGET LAPSED"), "{}", v.report);

    // What the judge filed before: every lapse credited, the machine blamed. With no record
    // of the floor's run, a starved run is still undecided, but no stop is called new.
    let v = judged(STARVED, None, Update::NONE);
    assert_eq!(row_of(&v, "aterm-gpu"), "NOT-VERIFIED(clock,<813,89)");
    note(&v, "BUDGET LAPSED: ");
    assert!(
        ![&v.report, &v.tail]
            .iter()
            .any(|t| t.contains("floor's run did not")),
        "{}{}",
        v.report,
        v.tail
    );
    let v = judged(QUIET, None, Update::NONE);
    assert_eq!(
        row_of(&v, "aterm-gpu"),
        "NOT-VERIFIED(spread,<813,1)",
        "not starved, so not the clock's; 1 short, and with no record of the floor's run no \
         stop is called new, so the prover's spread: {}",
        v.report
    );

    // The negative control: the one obligation the floor's run proved is stopped by a
    // clock in a starved run.
    let v = judged(STARVED, Some(&had), Update::RAISE);
    assert_eq!(row_of(&v, "aterm-gpu"), "NOT-VERIFIED(clock,<813,1)");
    assert_eq!((v.word, v.exit), ("COULD-NOT-RUN", 2));
    let lapsed = note(&v, "BUDGET LAPSED: ");
    assert!(
        lapsed.contains(&STARVED.said()) && lapsed.contains(&STARVED.unstarved()),
        "names the pace it had and the pace that decides it: {lapsed}"
    );
    assert!(v.tail.contains(&STARVED.unstarved()), "{}", v.tail);
    assert_eq!((v.ratchet, v.backstops), (None, None), "no floor is raised");

    // A stop every run has earns nothing, starved or not: beyond the prover's spread the
    // drop is the code's.
    let beyond = format!("aterm-gpu\t830\t4685\t{DRV}\n");
    let f = measured_check("aterm-gpu", GPU, Some(STARVED), true);
    let all_kept = kept_run("aterm-gpu", &all, 0);
    let v = judge(&f, Some(&beyond), Some(&all_kept), "R", "M", Update::NONE);
    assert_eq!(v.failed, ["aterm-gpu RATCHET-REGRESSION(<830) 812/4685"]);
    assert!(note(&v, "LOST: ").contains(&STARVED.said()), "{}", v.report);
}

/// TF-2: the prover is not reproducible (aterm-link 841 and 853 on one tree), so a run that
/// falls short of its own prover's floor by no more than [`spread`] and names no obligation
/// it lost is undecided, never `RATCHET-REGRESSION`, and sets no floor; beyond the spread,
/// or under another prover's floor, it is the code's. NEGATIVE CONTROL: the same run 1 short
/// with one obligation planted lost (a stop its floor's run did not have) is
/// `RATCHET-REGRESSION`, its `LOST` note naming it.
#[test]
fn a_shortfall_within_the_provers_spread_is_undecided_and_a_named_loss_is_not() {
    let rec = crate::trust_oblig::Record::parse(GPU.2).unwrap();
    let all = gated(&measured_check("aterm-gpu", GPU, None, true))
        .backstops
        .clone()
        .unwrap();
    let alike =
        |o: &crate::trust_oblig::Obligation| rec.rows.iter().filter(|r| r.key() == o.key()).count();
    let lost = rec
        .rows
        .iter()
        .find(|o| o.backstopped() && o.status == "timeout" && alike(o) == 1)
        .unwrap();
    let mut had = all.clone();
    had.remove(had.iter().position(|b| b.key == lost.key()).unwrap());
    let (every, planted) = (
        kept_run("aterm-gpu", &all, 0),
        kept_run("aterm-gpu", &had, 0),
    );
    let judged = |floor: u64, driver: &str, pace: Pace, run: &str, update: Update<'_>| {
        let f = measured_check("aterm-gpu", GPU, Some(pace), true);
        let row = format!("aterm-gpu\t{floor}\t4685\t{driver}\n");
        judge(&f, Some(&row), Some(run), "R", "M", update)
    };
    assert_eq!(
        (
            spread(813),
            spread(828),
            spread(829),
            spread(49),
            spread(50)
        ),
        (16, 16, 16, 0, 1)
    );

    // 812 against 813 and against 828, 16 short: the spread, so undecided.
    for floor in [813, 828] {
        let v = judged(floor, DRV, QUIET, &every, Update::RAISE);
        assert_eq!(
            row_of(&v, "aterm-gpu"),
            format!("NOT-VERIFIED(spread,<{floor},{})", floor - 812)
        );
        assert_eq!((v.word, v.exit), ("COULD-NOT-RUN", 2), "{}", v.report);
        assert!(v.failed.is_empty(), "{:?}", v.failed);
        assert_eq!(v.unverified, ["aterm-gpu"]);
        let within = note(&v, "WITHIN SPREAD: ");
        assert!(within.contains("the 16 (2% of the floor)"), "{within}");
        assert!(
            v.tail.contains("within the prover's run-to-run spread"),
            "{}",
            v.tail
        );
        assert_eq!(
            (v.ratchet, v.backstops),
            (None, None),
            "no floor is written"
        );
    }

    // 17 short of 829: beyond it, the code's.
    let v = judged(829, DRV, QUIET, &every, Update::NONE);
    assert_eq!(v.failed, ["aterm-gpu RATCHET-REGRESSION(<829) 812/4685"]);
    assert!(
        note(&v, "BEYOND SPREAD: ").contains("17 short"),
        "{}",
        v.report
    );

    // A floor under 50 has no spread: one proof short of its own prover's 4 is the code's,
    // and no BEYOND SPREAD note claims a spread of 0 was measured.
    let v = judge(
        &facts(vec![("alpha", gate("Summary: 3 proved (3 total)"))]),
        Some(&format!("alpha\t4\t3\t{DRV}\n")),
        None,
        "R",
        "M",
        Update::NONE,
    );
    assert_eq!(v.failed, ["alpha RATCHET-REGRESSION(<4) 3/3"]);
    assert!(!v.report.contains("SPREAD"), "{}", v.report);

    // A floor whose run was not kept (84 of the 142 floors, aterm-link's, aterm-messages' and
    // aterm-render's among them): no stop can be called new, so the 89 name no loss, and the
    // spread decides as for a run that named none.
    let v = judge(
        &measured_check("aterm-gpu", GPU, Some(QUIET), true),
        Some(&format!("aterm-gpu\t813\t4685\t{DRV}\n")),
        None,
        "R",
        "M",
        Update::NONE,
    );
    assert_eq!(row_of(&v, "aterm-gpu"), "NOT-VERIFIED(spread,<813,1)");
    assert!(
        note(&v, "WITHIN SPREAD: ").contains("was not kept"),
        "{}",
        v.report
    );
    let v = judge(
        &measured_check("aterm-gpu", GPU, Some(QUIET), true),
        Some(&format!("aterm-gpu\t829\t4685\t{DRV}\n")),
        None,
        "R",
        "M",
        Update::NONE,
    );
    assert_eq!(v.failed, ["aterm-gpu RATCHET-REGRESSION(<829) 812/4685"]);

    // Another prover's floor compares provers, not runs: no spread.
    let v = judged(
        813,
        "targo 1.99.0-dev (321aaeda7 2026-10-12)",
        QUIET,
        &every,
        Update::NONE,
    );
    assert_eq!(v.failed, ["aterm-gpu RATCHET-REGRESSION(<813) 812/4685"]);
    assert!(!v.report.contains("WITHIN SPREAD"), "{}", v.report);

    // A starved run: what it is credited with and the spread together leave it undecided,
    // and beyond both it is the code's.
    let v = judged(829, DRV, STARVED, &planted, Update::NONE);
    assert_eq!(row_of(&v, "aterm-gpu"), "NOT-VERIFIED(clock,<829,1)");
    assert!(
        note(&v, "BUDGET LAPSED: ").contains("the 16 it leaves is within the 16"),
        "{}",
        v.report
    );
    let v = judged(830, DRV, STARVED, &planted, Update::NONE);
    assert_eq!(v.failed, ["aterm-gpu RATCHET-REGRESSION(<830) 812/4685"]);

    // NEGATIVE CONTROL: 1 short, one obligation planted lost, not starved.
    let v = judged(813, DRV, QUIET, &planted, Update::RAISE);
    assert_eq!(v.failed, ["aterm-gpu RATCHET-REGRESSION(<813) 812/4685"]);
    assert!(note(&v, "LOST: ").contains(&lost.said()), "{}", v.report);
    assert!(!v.report.contains("WITHIN SPREAD"), "{}", v.report);
}

/// A starved run sets or raises a floor only when the machine can be credited with nothing
/// in it: what it proved is then what a run that was not starved proves.
#[test]
fn a_starved_run_sets_no_floor_the_machine_can_be_credited_with() {
    let alpha = |stops: usize, cancelled: u64| {
        facts(vec![(
            "alpha",
            Decision::Gate(Measured {
                rc: "0".into(),
                summary: "Summary: 3 proved (9 total)".into(),
                lapsed: stops as u64,
                cancelled,
                pace: Some(STARVED),
                backstops: Some(
                    (0..stops)
                        .map(|i| Backstop {
                            key: format!("{i:016x}"),
                            stop: "wall-backstop".into(),
                            said: format!("f{i}"),
                        })
                        .collect(),
                ),
                ..Measured::default()
            }),
        )])
    };
    let recorded = judge(&alpha(0, 0), Some(""), None, "R", "M", Update::RAISE);
    assert_eq!(row_of(&recorded, "alpha"), "FLOOR-RECORDED");
    assert_eq!(
        recorded.ratchet.as_deref(),
        Some(format!("alpha\t3\t9\t{DRV}\n").as_str())
    );
    assert_eq!(
        floor_run(recorded.backstops.as_deref(), "alpha"),
        Some(FloorRun::default()),
        "kept, and it had none"
    );
    for (stops, cancelled) in [(1, 0), (0, 1)] {
        let v = judge(
            &alpha(stops, cancelled),
            Some(""),
            None,
            "R",
            "M",
            Update::RAISE,
        );
        assert_eq!(row_of(&v, "alpha"), "NO-FLOOR", "{}", v.report);
        assert!(note(&v, "RE-BASE WITHHELD: alpha").contains(&STARVED.said()));
        assert!(!v.report.contains("NO FLOOR: "), "{}", v.report);
        assert_eq!((v.ratchet, v.backstops), (None, None));
    }

    let gpu = |pace: Pace| measured_check("aterm-gpu", GPU, Some(pace), true);
    let all = gated(&gpu(QUIET)).backstops.clone().unwrap();
    let met = format!("aterm-gpu\t812\t4685\t{DRV}\n");
    let v = judge(&gpu(STARVED), Some(&met), None, "R", "M", Update::RAISE);
    assert_eq!(
        (v.ratchet, v.backstops),
        (None, None),
        "its floor's run unknown, every stop is the machine's: {}",
        v.report
    );
    let had = kept_run("aterm-gpu", &all, 0);
    let v = judge(
        &gpu(STARVED),
        Some(&met),
        Some(&had),
        "R",
        "M",
        Update::RAISE,
    );
    assert_eq!(
        v.ratchet.as_deref(),
        Some(met.as_str()),
        "every stop recurs, so none is the machine's: {}",
        v.report
    );

    let v = judge(
        &gpu(QUIET),
        Some(""),
        Some(&kept_run("aterm-render", &[], 0)),
        "R",
        "M",
        Update::RAISE,
    );
    assert_eq!(row_of(&v, "aterm-gpu"), "FLOOR-RECORDED");
    assert_eq!(v.ratchet.as_deref(), Some(met.as_str()));
    let kept = v.backstops.unwrap();
    assert_eq!(
        floor_run(Some(&kept), "aterm-gpu").map(|r| (r.stops.len(), r.cancelled)),
        Some((89, 0)),
        "the run's own stops, beside its floor"
    );
    assert!(floor_run(Some(&kept), "aterm-render").is_some());
    assert!(kept.starts_with(BACKSTOPS_HEADER));
    let v = judge(
        &gpu(STARVED),
        Some(&met),
        Some(&kept),
        "R",
        "M",
        Update::NONE,
    );
    assert_eq!(v.word, "PASS", "{}", v.report);
}

/// aterm-render's RSS stops are the machine's as its clock stops are; with no record the
/// counts stand in, rss-backstop's among them, and a floor is not raised over stops nothing
/// names.
#[test]
fn an_rss_stop_is_credited_as_a_clock_stop_is() {
    let f = measured_check("aterm-render", RENDER, Some(STARVED), true);
    let stops = gated(&f).backstops.clone().unwrap();
    let wall: Vec<Backstop> = stops
        .iter()
        .filter(|b| b.stop == "wall-backstop")
        .cloned()
        .collect();
    let judged = |f: &Facts, floor: u64, had: &[Backstop]| {
        judge(
            f,
            Some(&format!("aterm-render\t{floor}\t20856\t{DRV}\n")),
            Some(&kept_run("aterm-render", had, 0)),
            "R",
            "M",
            Update::NONE,
        )
    };
    // What is credited, and the prover's spread of the floor, cover the shortfall.
    let edge = |covered: u64| (covered..).find(|f| covered + spread(*f) < *f).unwrap();
    assert_eq!(edge(1730 + 44), 1811, "1774 and the 36 of 1810");
    let v = judged(&f, 1810, &wall);
    assert_eq!(row_of(&v, "aterm-render"), "NOT-VERIFIED(clock,<1810,44)");
    let v = judged(&f, 1811, &wall);
    assert_eq!(row_of(&v, "aterm-render"), "RATCHET-REGRESSION(<1811)");
    let v = judged(&f, 1730 + 44, &stops);
    assert_eq!(
        row_of(&v, "aterm-render"),
        "RATCHET-REGRESSION(<1774)",
        "the control: the floor's run had its RSS stops too"
    );

    let bare = measured_check("aterm-render", RENDER, Some(STARVED), false);
    assert_eq!(edge(1730 + 747 + 715 - 6), 3252, "3186 and the 65 of 3251");
    let v = judged(&bare, 3251, &wall);
    assert_eq!(row_of(&v, "aterm-render"), "NOT-VERIFIED(clock,<3251,1456)");
    let v = judged(&bare, 3252, &wall);
    assert_eq!(row_of(&v, "aterm-render"), "RATCHET-REGRESSION(<3252)");
    let quiet = measured_check("aterm-render", RENDER, Some(QUIET), false);
    let v = judge(
        &quiet,
        Some(&format!("aterm-render\t1730\t20856\t{DRV}\n")),
        None,
        "R",
        "M",
        Update::RAISE,
    );
    assert_eq!(v.ratchet, None, "{}", v.report);
    note(&v, "RE-BASE WITHHELD: aterm-render");
}

/// A trustd refusal excuses only itself in a run that was not starved: what a backstop
/// stopped there is the code's, so it cannot help cover a shortfall the refusals do not.
#[test]
fn a_refusal_covers_only_itself_in_a_run_that_was_not_starved() {
    let run = |pace: Pace, refused: u64| {
        facts(vec![(
            "aterm-gpu",
            Decision::Gate(Measured {
                rc: "1".into(),
                summary: "Summary: 812 proved (4685 total)".into(),
                refused,
                refused_why: "trustd unreachable: 1".into(),
                lapsed: 89,
                pace: Some(pace),
                ..Measured::default()
            }),
        )])
    };
    let floor = format!("aterm-gpu\t850\t4685\t{DRV}\n");
    let judged = |f: &Facts| judge(f, Some(&floor), None, "R", "M", Update::NONE);
    let v = judged(&run(QUIET, 1));
    assert_eq!(
        v.failed,
        ["aterm-gpu RATCHET-REGRESSION(<850) 812/4685"],
        "{}",
        v.report
    );
    assert_eq!(
        row_of(&judged(&run(STARVED, 1)), "aterm-gpu"),
        "NOT-VERIFIED(trustd)"
    );
    assert_eq!(
        row_of(&judged(&run(QUIET, 38)), "aterm-gpu"),
        "NOT-VERIFIED(trustd)",
        "the refusals alone cover it"
    );
}

#[test]
fn a_pace_is_starved_below_four_fifths_of_its_wall_or_above_the_cores() {
    let p = |cpu_ms, wall_ms, load| Pace {
        cpu_ms,
        wall_ms,
        load,
        cores: 14,
        threads: None,
    };
    assert!(!p(800, 1000, Some(1400)).starved());
    assert!(p(799, 1000, Some(100)).starved());
    assert!(p(1000, 1000, Some(1401)).starved());
    assert!(!p(5000, 1000, None).starved(), "a load not read is no load");
    assert!(!p(0, 0, None).starved());
    for pace in [STARVED, QUIET, p(1, 2, None)] {
        assert_eq!(Pace::parse(&pace.text()), Some(pace));
    }
    assert_eq!(
        p(1, 2, None).said(),
        "CPU/wall 0.50, load unread on 14 cores"
    );
    for bad in [
        "",
        "-",
        "1 2 3",
        "1 2 x 4",
        "1 2 +3 4",
        "1 2 3 4 5",
        "1 2 3 4 5 x",
        "1 2 3 4 5 6 7",
    ] {
        assert_eq!(Pace::parse(bad), None, "{bad:?}");
    }
}

/// aterm-font under 20565c325 on this Mac (2026-10-25): 27905 s of CPU in 3861 s at a mean
/// load of 41.43 on 14 cores, most of it its own verifier's runnable threads.
#[test]
fn a_pace_whose_threads_were_read_is_starved_by_their_share_alone() {
    let at = |load, threads| Pace {
        cpu_ms: 27_904_710,
        wall_ms: 3_861_090,
        load: Some(load),
        cores: 14,
        threads,
    };
    let th = |ran_ms, runnable_ms| {
        Some(Threads {
            ran_ms,
            runnable_ms,
            cores: None,
        })
    };
    let ran = at(4143, th(800, 1000));
    assert!(
        !ran.starved(),
        "threads that ran are not starved by the load: {}",
        ran.said()
    );
    assert!(
        at(200, th(799, 1000)).starved(),
        "threads that waited are, at any load"
    );
    // The negative control: the same run with its threads unread is judged by the load.
    assert!(at(4143, None).starved());
    assert!(!at(200, None).starved());
    assert!(
        at(4143, th(0, 0)).starved(),
        "a read with no runnable time says nothing"
    );
    assert_eq!(Pace::parse(&ran.text()), Some(ran));
    assert_eq!(Pace::parse("1 2 - 4 5 6").and_then(|p| p.threads), th(5, 6));
    assert_ne!(ran.unstarved(), at(4143, None).unstarved());
    // The lane's run of it at `--worker-threads 3` (2026-10-25), as its `.pace` mark says.
    let lane = Pace::parse("25022638 3675162 4195 14 24965019 43582620").unwrap();
    assert!(lane.starved());
    assert_eq!(
        lane.said(),
        "CPU/wall 6.81, load 41.95 on 14 cores, threads ran 0.57 of their runnable time, 11.86 of them runnable on average"
    );
}

/// FONT-ECORE-SHARE: a thread on an efficiency core runs slower than on a performance core,
/// so the share counts its time there at the speed the check's instructions ran there. The
/// probe run of 2026-10-27 on this M4 Pro (6 threads at a load of about 23): threads ran
/// 0.907 of their runnable time, 0.742 of it on performance cores, and an efficiency core
/// retired 0.5 of a performance core's instructions a millisecond; 11.1% of its tasks lapsed
/// a deadline that none lapsed on performance cores alone. Weighed, it ran 0.79 of its
/// runnable time at a performance core's speed: starved. NEGATIVE CONTROL: the same pace with
/// no split read is unstarved, as the share alone judged it. Threads that ran on performance
/// cores alone keep their share; with no time on a performance core to compare against, an
/// efficiency core's time counts in full and the note says so.
#[test]
fn an_efficiency_cores_time_counts_at_its_speed() {
    let at = |cores| Pace {
        cpu_ms: 5_442,
        wall_ms: 1_000,
        load: Some(2300),
        cores: 14,
        threads: Some(Threads {
            ran_ms: 907,
            runnable_ms: 1000,
            cores,
        }),
    };
    let split = |p_ms, instructions, p_instructions| {
        Some(Cores {
            p_ms,
            instructions,
            p_instructions,
        })
    };
    // 673 ms on performance cores at 1000 instructions a ms, 234 on efficiency cores at 500.
    let probe = at(split(673, 673_000 + 117_000, 673_000));
    assert_eq!(probe.threads.unwrap().worked_ms(), 790);
    assert!(probe.starved(), "{}", probe.said());
    assert_eq!(
        probe.said(),
        "CPU/wall 5.44, load 23.00 on 14 cores, threads ran 0.91 of their runnable time (0.79 at a performance core's speed: 0.26 of it on efficiency cores, at 0.50 of that speed), 1.00 of them runnable on average"
    );
    assert!(probe.unstarved().contains("efficiency core"));
    assert!(!at(None).starved(), "the share alone: {}", at(None).said());
    let p_only = at(split(907, 907_000, 907_000));
    assert!(!p_only.starved(), "{}", p_only.said());
    assert!(!p_only.said().contains("efficiency"), "{}", p_only.said());
    let e_only = at(split(0, 450_000, 0));
    assert!(!e_only.starved(), "{}", e_only.said());
    assert!(
        e_only.said().contains("counted at full speed"),
        "{}",
        e_only.said()
    );
    // Faster on an efficiency core than the performance cores' rate counts no more than ran.
    assert_eq!(
        at(split(673, 5_000_000, 673_000))
            .threads
            .unwrap()
            .worked_ms(),
        907
    );
    for p in [probe, at(None), p_only] {
        assert_eq!(Pace::parse(&p.text()), Some(p), "{}", p.text());
    }
    assert_eq!(probe.text(), "5442 1000 2300 14 907 1000 673 790000 673000");
    for bad in [
        "1 2 3 4 5 6 7 8",
        "1 2 3 4 5 6 7 8 x",
        "1 2 3 4 5 6 7 8 9 10",
    ] {
        assert_eq!(Pace::parse(bad), None, "{bad:?}");
    }
}

#[test]
fn the_floors_backstops_keep_each_crates_own_rows() {
    let b = |key: &str, stop: &str| Backstop {
        key: key.into(),
        stop: stop.into(),
        said: format!("f{key}"),
    };
    let run = |stops: &[Backstop], cancelled| FloorRun {
        stops: stops.to_vec(),
        cancelled,
    };
    let text = kept_run("beta", &[b("2", "rss-backstop")], 3);
    let text = backstops_rewritten(Some(&text), "alpha", &run(&[b("1", "wall-backstop")], 0));
    assert_eq!(
        text,
        format!(
            "{BACKSTOPS_HEADER}\nalpha\t-\tcancelled\t0\nalpha\t1\twall-backstop\tf1\nbeta\t-\tcancelled\t3\nbeta\t2\trss-backstop\tf2\n"
        )
    );
    assert_eq!(
        floor_run(Some(&text), "beta"),
        Some(run(&[b("2", "rss-backstop")], 3))
    );
    let text = backstops_rewritten(Some(&text), "beta", &FloorRun::default());
    assert_eq!(
        floor_run(Some(&text), "beta"),
        Some(FloorRun::default()),
        "kept, and it had none"
    );
    assert_eq!(
        floor_run(Some(&text), "alpha").map(|r| r.stops.len()),
        Some(1)
    );
    assert_eq!(floor_run(Some(&text), "gamma"), None, "never kept: unknown");
    assert_eq!(floor_run(Some(&text), "alph"), None);
    assert_eq!(floor_run(None, "alpha"), None);
    let headless = text.replace("alpha\t-\tcancelled\t0\n", "");
    assert_eq!(
        floor_run(Some(&headless), "alpha"),
        None,
        "stops without a head"
    );
    let unread = text.replace("alpha\t-\tcancelled\t0", "alpha\t-\tcancelled\tsome");
    assert_eq!(floor_run(Some(&unread), "alpha"), None);

    let mut f = measured_check("aterm-gpu", GPU, Some(QUIET), true);
    let record = f.to_record();
    let line = record.lines().find(|l| l.starts_with("backstop ")).unwrap();
    assert_eq!(
        Facts::parse(&record.replacen(&format!("{line}\n"), "", 1)),
        None
    );
    assert_eq!(
        Facts::parse(&record.replace("backstop aterm-gpu\t", "backstop aterm-gfx\t")),
        None
    );
    if let Decision::Gate(m) = &mut f.crates[0].decision {
        m.backstops = None;
    }
    assert!(!f.to_record().contains("\nbackstop "));
    assert_eq!(Facts::parse(&f.to_record()), Some(f));
}

/// No target is published until every staged write succeeds; a missing parent is a real
/// staging failure, independent of how the writer names its scratch.
#[test]
fn the_floor_files_are_written_together_or_not_at_all() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let (floors, stops) = (
        dir.path().join("floors.tsv"),
        dir.path().join("backstops.tsv"),
    );
    fs::write(&floors, "old floors\n").unwrap();
    fs::write(&stops, "old stops\n").unwrap();
    let names = || {
        fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<BTreeSet<_>>()
    };
    let before = names();
    let blocked = dir.path().join("missing-parent/blocked.tsv");
    let err = write_floor_files(&[
        (&floors, Some("new floors\n")),
        (&blocked, Some("x\n")),
        (&stops, Some("new stops\n")),
    ])
    .unwrap_err();
    assert!(err.contains("blocked.tsv"), "{err}");
    assert_eq!(fs::read_to_string(&floors).unwrap(), "old floors\n");
    assert_eq!(fs::read_to_string(&stops).unwrap(), "old stops\n");
    assert_eq!(names(), before, "a failed pass leaves no scratch");
    write_floor_files(&[(&floors, Some("new floors\n")), (&stops, None)]).unwrap();
    assert_eq!(fs::read_to_string(&floors).unwrap(), "new floors\n");
    assert_eq!(fs::read_to_string(&stops).unwrap(), "old stops\n");
    assert_eq!(names(), before, "a successful pass leaves no scratch");
}

/// An existing temporary-looking name is somebody else's file or directory. A successful
/// update keeps its identity and bytes, even when it is linked to another payload.
#[cfg(unix)]
#[test]
fn floor_updates_preserve_occupied_legacy_scratch() {
    use std::os::unix::fs::MetadataExt as _;

    let ordinary = aterm_tempfile::tempdir().unwrap();
    let floor = ordinary.path().join("memory.tsv");
    write_floor_files(&[(&floor, Some("ordinary update\n"))]).unwrap();
    assert_eq!(fs::read_to_string(&floor).unwrap(), "ordinary update\n");

    let mut outcomes = Vec::new();
    for kind in ["regular", "hardlink", "symlink", "directory"] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let floor = dir.path().join("memory.tsv");
        let occupied = floor.with_extension(format!("tsv.{}", std::process::id()));
        let payload = dir.path().join("unrelated-payload");
        fs::write(&floor, "old memory\n").unwrap();
        fs::write(&payload, "keep this payload\n").unwrap();
        match kind {
            "regular" => fs::write(&occupied, "keep this name\n").unwrap(),
            "hardlink" => fs::hard_link(&payload, &occupied).unwrap(),
            "symlink" => std::os::unix::fs::symlink(&payload, &occupied).unwrap(),
            "directory" => fs::create_dir(&occupied).unwrap(),
            _ => unreachable!(),
        }
        let identity = |path: &Path| {
            fs::symlink_metadata(path)
                .ok()
                .map(|meta| (meta.dev(), meta.ino(), meta.mode(), meta.len()))
        };
        let before = identity(&occupied);
        let bytes = fs::read(&occupied).ok();
        let link = fs::read_link(&occupied).ok();
        let updated = write_floor_files(&[(&floor, Some("new memory\n"))]);
        outcomes.push((
            kind,
            updated.is_ok(),
            identity(&occupied) == before
                && fs::read(&occupied).ok() == bytes
                && fs::read_link(&occupied).ok() == link,
            fs::read_to_string(&payload).unwrap() == "keep this payload\n",
            fs::read_to_string(&floor).unwrap() == "new memory\n",
        ));
    }
    assert!(
        outcomes
            .iter()
            .all(|(_, wrote, kept, payload, target)| { *wrote && *kept && *payload && *target }),
        "occupied names must survive each successful update: {outcomes:?}"
    );
}

/// A failed first stage never acquired a later scratch name; cleanup cannot unlink it.
#[test]
fn floor_staging_failure_preserves_an_unvisited_occupied_name() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let ordinary = dir.path().join("ordinary.tsv");
    write_floor_files(&[(&ordinary, Some("ordinary update\n"))]).unwrap();
    assert_eq!(fs::read_to_string(&ordinary).unwrap(), "ordinary update\n");
    let missing = dir.path().join("missing-parent/floors.tsv");
    let later = dir.path().join("backstops.tsv");
    let occupied = later.with_extension(format!("tsv.{}", std::process::id()));
    fs::write(&later, "old stops\n").unwrap();
    fs::write(&occupied, "keep this unvisited name\n").unwrap();
    let before: BTreeSet<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert!(
        write_floor_files(&[
            (&missing, Some("new floors\n")),
            (&later, Some("new stops\n"))
        ])
        .is_err()
    );
    assert_eq!(fs::read_to_string(&later).unwrap(), "old stops\n");
    assert_eq!(
        fs::read_to_string(&occupied).ok().as_deref(),
        Some("keep this unvisited name\n"),
        "cleanup must preserve a name it never acquired"
    );
    let after: BTreeSet<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(after, before, "failed staging changes no other name");
}

#[test]
fn a_check_targo_would_not_start_is_undecided() {
    let m = log(&format!(
        "error: {TRUSTD_REFUSED}: another trustd owns the socket\n"
    ));
    assert_eq!(m.launch, "owned");
    let m = log(&format!("error: {TRUSTD_REFUSED}\n{TRUSTD_DIRTY}\n"));
    assert_eq!(m.launch, "dirty");
    assert_eq!(
        log(&format!(
            "{TRUSTD_REFUSED}\n  Summary: 1 proved (1 total)\n"
        ))
        .launch,
        "",
        "a check that reached its Summary started"
    );
    let refused = |launch: &str, held: &str| {
        facts(vec![(
            "alpha",
            Decision::Gate(Measured {
                rc: "1".into(),
                launch: launch.into(),
                held: held.into(),
                held_what: "a trustd of another build serves /tmp/t.sock".into(),
                ..Measured::default()
            }),
        )])
    };
    let v = judge(
        &refused("owned", "foreign"),
        Some(""),
        None,
        "R",
        "M",
        Update::NONE,
    );
    assert_eq!((v.word, v.exit), ("COULD-NOT-RUN", 2));
    assert_eq!(v.unverified, ["alpha"]);
    let v = judge(
        &refused("dirty", ""),
        Some(""),
        None,
        "R",
        "M",
        Update::NONE,
    );
    assert_eq!((v.word, v.exit), ("COULD-NOT-RUN", 2));
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn the_recovery_command_survives_a_store_path_with_a_space() {
    let r = trustd_remedy("dirty", "/tmp/t.sock", "/a b/bin/trustd");
    assert!(
        r.contains(
            "`'/a b/bin/trustd' --recover-after-crash --confirm-no-solvers --socket /tmp/t.sock`"
        ),
        "{r}"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn the_recovery_probe_observes_a_matching_owned_ancestor() {
    use std::os::unix::fs::MetadataExt as _;
    use std::time::Instant;

    const CHILD: &str = "ATERM_TEST_PGREP_ANCESTOR";
    if let Some(path) = std::env::var_os(CHILD) {
        let path = PathBuf::from(path);
        let image = std::env::current_exe().unwrap();
        assert!(aterm_uds::test_binary::is_test_binary(&image));
        let own = std::fs::metadata(&image).unwrap();
        let linked = std::fs::metadata(&path).unwrap();
        assert_eq!((own.dev(), own.ino()), (linked.dev(), linked.ino()));
        let name = path.file_name().unwrap().to_str().unwrap();
        let probe = |options: &str| {
            aterm_uds::spawnfd::command("/usr/bin/pgrep")
                .args([options, name])
                .output()
                .expect("owned process-name probe")
        };
        let matches_self = |out: &std::process::Output| {
            out.status.success()
                && String::from_utf8_lossy(&out.stdout).lines().any(|line| {
                    line.split_whitespace()
                        .next()
                        .and_then(|pid| pid.parse::<u32>().ok())
                        == Some(std::process::id())
                })
        };
        assert!(matches_self(&probe("-alx")), "owned ancestor is observable");
        let old = probe("-lx");
        assert_eq!(old.status.code(), Some(1));
        assert!(old.stdout.is_empty());
        let current = probe(trustd_probe_options().expect("macOS probe"));
        println!(
            "OWNED_ANCESTOR_PID={} PROBE={}",
            std::process::id(),
            String::from_utf8_lossy(&current.stdout).trim()
        );
        assert!(
            matches_self(&current),
            "the declared probe must see its matching ancestor"
        );
        return;
    }

    let scratch = aterm_tempfile::Builder::new()
        .prefix("trustd-probe-ancestor-")
        .tempdir()
        .unwrap();
    let image = std::env::current_exe().unwrap();
    let (_, hash) = image
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .and_then(|name| name.rsplit_once('-'))
        .expect("original Cargo test image");
    let named = scratch
        .path()
        .join(format!("atpa{}-{hash}", std::process::id()));
    assert!(
        aterm_uds::test_binary::is_test_binary(&named),
        "the ancestor fixture must retain Cargo-name isolation before it starts"
    );
    let identity = |metadata: &std::fs::Metadata| {
        (
            metadata.dev(),
            metadata.ino(),
            metadata.nlink(),
            metadata.mtime(),
            metadata.mtime_nsec(),
            metadata.ctime(),
            metadata.ctime_nsec(),
        )
    };
    let before = identity(&std::fs::metadata(&image).unwrap());
    std::fs::copy(&image, &named).unwrap();
    assert_eq!(
        identity(&std::fs::metadata(&image).unwrap()),
        before,
        "fixture preparation must not mutate its active source image"
    );
    let mut scratch = aterm_uds::deadline::KeptUntilRan::new(scratch.keep());
    let test = "trust_facts::tests::the_recovery_probe_observes_a_matching_owned_ancestor";
    let mut child = aterm_uds::spawnfd::command(&named);
    child
        .args(["--exact", test, "--nocapture"])
        .env(CHILD, &named);
    let out = aterm_uds::deadline::output_until(
        &mut child,
        Instant::now() + aterm_uds::deadline::FIRST_EXEC_HANG,
    )
    .expect("owned ancestor fixture finishes and is reaped");
    if out.status.code().is_some() {
        scratch.ran();
    }
    println!("{}", String::from_utf8_lossy(&out.stdout));
    assert!(out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("OWNED_ANCESTOR_PID="));
}

#[test]
fn a_remeasure_folds_only_its_own_key() {
    let prov = |key: &str| Provenance {
        key: key.into(),
        tree_sans_floors: "t".into(),
        prover: "p".into(),
        lane: LANE_WHOLE_TREE.into(),
        head: "h1".into(),
        date: "d1".into(),
        ..Provenance::default()
    };
    let mut base = facts(vec![
        ("alpha", gate("Summary: 1 proved (3 total)")),
        ("beta", gate("Summary: 2 proved (2 total)")),
    ]);
    base.provenance = Some(prov("k"));
    let mut again = facts(vec![("alpha", gate("Summary: 3 proved (3 total)"))]);
    again.provenance = Some(Provenance {
        head: "h2".into(),
        date: "d2".into(),
        ..prov("k")
    });
    let done = remeasured(&base, &again).unwrap();
    assert_eq!(done.crates[0].decision, gate("Summary: 3 proved (3 total)"));
    assert_eq!(done.crates[1], base.crates[1]);
    let p = done.provenance.unwrap();
    assert_eq!(
        (p.head.as_str(), p.remeasured),
        ("h1", vec!["d2 h2 alpha".to_string()])
    );
    let mut other_key = again.clone();
    other_key.provenance = Some(prov("k2"));
    assert!(
        remeasured(&base, &other_key)
            .unwrap_err()
            .contains("two keys")
    );
    let mut other_host = again.clone();
    other_host.host.mem = 1;
    assert!(remeasured(&base, &other_host).unwrap_err().contains("host"));
    let mut other_ceiling = again.clone();
    other_ceiling.host.uncapped = !again.host.uncapped;
    assert!(
        remeasured(&base, &other_ceiling)
            .unwrap_err()
            .contains("verifier ceiling")
    );
    let mut other_driver = again.clone();
    other_driver.driver_id = "targo 2".into();
    assert!(
        remeasured(&base, &other_driver)
            .unwrap_err()
            .contains("driver")
    );
    let mut stranger = again.clone();
    stranger.crates = vec![Crate::new("gamma", gate("Summary: 1 proved (1 total)"))];
    assert!(remeasured(&base, &stranger).unwrap_err().contains("gamma"));
    let text = remeasured(&base, &again).unwrap().to_record();
    assert!(text.contains("\nremeasured d2 h2 alpha\n"), "{text}");
    assert_eq!(
        Facts::parse(&text).unwrap().provenance.unwrap().remeasured,
        ["d2 h2 alpha"]
    );
}

fn host(gib: u64) -> Host {
    Host {
        mem: gib * 1_073_741_824,
        ..Host::default()
    }
}

fn mem(ram: &str, at: &str, need: &str, bound: &str) -> MemRow {
    MemRow {
        f: "16.0".into(),
        ram: ram.into(),
        at: at.into(),
        need: need.into(),
        bound: bound.into(),
        when: "m".into(),
    }
}

#[test]
fn a_need_printed_at_its_own_hosts_ceiling_is_refused_and_names_what_fits() {
    let cli = [mem("8", "4.0", "2.1", "="), mem("24", "4.0", "3.0", "=")];
    let on = |gib: u64| {
        let h = host(gib);
        let row = row_for(&cli, &h).unwrap().clone();
        (memory_refusal(&h, "aterm-cli", true, &row, None), row)
    };
    let (at24, row) = on(24);
    assert_eq!(row.ram, "24");
    let at24 = at24.expect("a 24 GiB host refuses it up front");
    assert_eq!(at24.token, "~3.0");
    assert_eq!(at24.sure, Some(32));
    let (at32, row) = on(32);
    assert_eq!((at32, row.ram.as_str()), (None, "24"));
    assert!(!fits(&row, &host(32), "4.0"));
    let (at16, row) = on(16);
    assert_eq!(row.ram, "8");
    assert!(at16.is_some());
    let (at8, _) = on(8);
    assert!(at8.is_some());
}

#[test]
fn a_need_from_another_ram_is_read_as_the_bound_it_is() {
    let regex = [mem("24", "3.0", "1.0", "=")];
    let census = [mem("8", "4.0", "1.3", "=")];
    for gib in [8, 16, 24, 32, 128] {
        let h = host(gib);
        let c = h.ceiling().unwrap();
        let row = row_for(&regex, &h).unwrap();
        assert_eq!(
            memory_refusal(&h, "aterm-regex", true, row, None),
            None,
            "{gib}"
        );
        assert_eq!(fits(row, &h, &c), gib <= 24, "regex {gib}");
        let row = row_for(&census, &h).unwrap();
        assert!(!fits(row, &h, &c), "census {gib}");
        assert_eq!(
            memory_refusal(&h, "aterm-census", true, row, None).is_some(),
            gib == 8,
            "census {gib}"
        );
    }
    let verify = mem("24", "3.0", "3.2", "=");
    assert!(fits(&verify, &host(24), "3.0"));
    assert_eq!(memory_refusal(&host(24), "v", true, &verify, None), None);
    let gui = [mem("128", "16.0", "16.4", ">")];
    assert!(memory_refusal(&host(24), "g", true, &gui[0], None).is_some());
    assert_eq!(row_for(&gui, &host(24)).unwrap().ram, "128");
    let both = [mem("8", "4.0", "2.1", "="), mem("24", "4.0", "3.0", "=")];
    assert_eq!(row_for(&both, &Host::default()).unwrap().ram, "24");
    assert_eq!(
        memory_refusal(&Host::default(), "x", true, &both[1], None),
        None
    );
}

#[test]
fn the_memory_record_is_held_to_its_shape() {
    let ok = "# c\nalpha\t16.0\t24\t4.0\t3.0\t=\tm101\nalpha\t16.0\t8\t4.0\t2.1\t=\tA18\nbeta\t16.0\t-\t-\t-\t-\t-\n";
    let rec = parse_memory(ok, "R").unwrap();
    assert_eq!(rec["alpha"].len(), 2);
    if let Err(e) = parse_memory(include_str!("../trust/memory.tsv"), MEMORY) {
        panic!("the committed record does not parse: {e}");
    }
    assert_eq!(
        rec["beta"],
        [MemRow {
            f: "16.0".into(),
            ..MemRow::default()
        }]
    );
    for (text, rule) in [
        (
            "alpha\t16.0\t24\t4.0\t3.0\t=\tx\nalpha\t1.0\t8\t1.0\t0.3\t=\tx\n",
            "two floor ceilings",
        ),
        (
            "alpha\t16.0\t24\t4.0\t3.0\t=\tx\nalpha\t16.0\t24.0\t3.0\t3.1\t>\tx\n",
            "two rows measured on 24.0 GiB",
        ),
        (
            "alpha\t16.0\t-\t-\t-\t-\t-\nalpha\t16.0\t24\t3.0\t1.0\t=\tx\n",
            "a row with no need beside another",
        ),
        (
            "alpha\t16.0\t-\t3.0\t1.0\t=\tx\n",
            "a need names the RAM it was measured on",
        ),
        (
            "alpha\t16.0\t24\t-\t-\t-\tx\n",
            "a need names the RAM it was measured on",
        ),
        (
            "alpha\t16.0\t-\t3.0\t-\t-\tx\n",
            "the run ceiling '3.0' with no need",
        ),
        ("alpha\t16.0\tlots\t3.0\t1.0\t=\tx\n", "the host RAM 'lots'"),
        ("alpha\t16.0\t24\tx\t1.0\t=\tx\n", "the run ceiling 'x'"),
    ] {
        assert!(parse_memory(text, "R").is_err(), "{rule}");
    }
}

#[test]
fn a_run_writes_the_need_it_measured_to_its_own_ram() {
    let census = |peak: &str, ceil: &str, exhausted: bool| Census {
        peak: Some(peak.into()),
        ceil: Some(ceil.into()),
        exhausted,
        ..Census::default()
    };
    let a18 = mem("8", "4.0", "2.1", "=");
    let new = mem_learn(
        &a18,
        "16.0",
        &census("3.0", "3.0", true),
        "2026-10-01",
        &host(24),
    )
    .unwrap();
    assert_eq!(
        (
            new.ram.as_str(),
            new.at.as_str(),
            new.need.as_str(),
            new.bound.as_str(),
            new.when.as_str()
        ),
        ("24", "3.0", "3.0", ">", "2026-10-01")
    );
    let own = mem("24", "4.0", "3.0", "=");
    assert_eq!(
        mem_learn(&own, "16.0", &census("2.9", "3.0", true), "d", &host(24)),
        None
    );
    let more = mem_learn(&own, "16.0", &census("3.3", "3.0", true), "d", &host(24)).unwrap();
    assert_eq!(
        (more.ram.as_str(), more.need.as_str(), more.bound.as_str()),
        ("24", "3.3", ">")
    );
    let unknown = mem_learn(
        &a18,
        "4.0",
        &census("3.0", "3.0", false),
        "d",
        &Host::default(),
    );
    assert_eq!(
        unknown,
        Some(MemRow {
            f: "4.0".into(),
            ..a18.clone()
        })
    );
    let text = "# h\naterm-cli\t16.0\t8\t4.0\t2.1\t=\tA18\nbeta\t16.0\t-\t-\t-\t-\t-\n";
    let mut learned = new.clone();
    learned.f = "4.0".into();
    let beta = mem_learn(
        &MemRow {
            f: "16.0".into(),
            ..MemRow::default()
        },
        "16.0",
        &census("0.6", "16.0", false),
        "d",
        &host(128),
    )
    .unwrap();
    let out = memory_rewritten(
        text,
        &[("aterm-cli".into(), learned), ("beta".into(), beta)],
    );
    assert_eq!(
        out,
        "# h\naterm-cli\t4.0\t8\t4.0\t2.1\t=\tA18\naterm-cli\t4.0\t24\t3.0\t3.0\t>\t2026-10-01\nbeta\t16.0\t128\t16.0\t0.6\t=\td\n"
    );
    assert!(parse_memory(&out, "R").is_ok());
}

#[test]
fn the_facts_carry_each_rows_ram_and_run_ceiling() {
    let mut f = facts(vec![("aterm-cli", Decision::Memory(None))]);
    f.crates[0].memory = mem("24", "4.0", "3.0", "=");
    f.host = host(24);
    let text = f.to_record();
    assert!(
        text.contains("\ncrate aterm-cli\tmemory\t16.0\t24\t4.0\t3.0\t=\tm\t-\t-\t-\t-\t-\n"),
        "{text}"
    );
    assert_eq!(Facts::parse(&text), Some(f.clone()));
    assert_eq!(
        Facts::parse(&text.replace(FACTS_MAGIC, "aterm-trust facts 4")),
        None
    );
    // Whether the prover took a ceiling is kept; a record from before the probe had one.
    assert!(text.contains("\ndriver-ceiling yes\n"), "{text}");
    let mut open = f.clone();
    open.host.uncapped = true;
    assert_eq!(Facts::parse(&open.to_record()), Some(open));
    assert_eq!(
        Facts::parse(&text.replace("driver-ceiling yes\n", "")),
        Some(f.clone())
    );
    assert_eq!(
        Facts::parse(&text.replace("driver-ceiling yes", "driver-ceiling maybe")),
        None
    );
    let v = judge(
        &f,
        Some(&format!("aterm-cli\t75\t443\t{DRV}\n")),
        None,
        "R",
        "M",
        Update::NONE,
    );
    assert_eq!(v.word, "NARROWED");
}

const REFUSED_LOG: &str = include_str!("../tests/fixtures/trust/refused-path-umask-002.log");

const REFUSED_DIR: &str = "/home/u/.cache/aterm-judge-fixture.wGWg0z/g";

const REFUSED_TMP: &str = "error: verified runtime-library directory path component `/tmp` is group- or world-writable (uid=0, mode=0o1777); any local principal could substitute the executed object between authorization and execution\n\
                           targo trust: error: verification did not run: no per-function TRUST_JSON transport row, authenticated coverage summary, or terminal Cargo target inventory was collected\n";

const REFUSED_TRUSTD: &str = "targo trust: error: could not establish the selected same-sysroot trustd memory authority at /tmp/trustd-runtime-locks-1000/trustd.sock: could not authorize the same-sysroot trustd executable /home/u/tc/bin/trustd: trustd executable ancestor is foreign-owned or cross-user writable: /home/u/tc; refusing to launch a split-authority Cargo fan-out. Confirm the sibling trustd at `/home/u/tc/bin/trustd` is installed/executable and stop any incompatible live daemon.\n";

fn writable(owner: Option<u32>, mode: u32) -> Refused {
    Refused::Writable {
        owner,
        mode: Some(mode),
        link: None,
    }
}

fn refusal(path: &str, why: Refused) -> PathRefusal {
    PathRefusal {
        path: path.into(),
        why,
    }
}

#[test]
fn a_refused_path_is_read_in_targos_own_words() {
    let m = Measured::of_log(REFUSED_LOG.as_bytes(), "2");
    assert!(m.transport && m.summary.is_empty(), "{m:?}");
    assert_eq!(
        m.path_refused,
        [refusal(REFUSED_DIR, writable(Some(1000), 0o775))]
    );
    let (head, _) = REFUSED_LOG
        .split_once("error: verified runtime-library directory")
        .unwrap();
    let foreign = Refused::Foreign {
        owner: 501,
        me: 1000,
    };
    let cases = [
        (REFUSED_TMP.to_string(), refusal("/tmp", writable(Some(0), 0o1777))),
        (
            "error: failed to run custom build command for `x`\n\nCaused by:\n  verified tool launcher path component `/opt/trust` is owned by uid=501, which is neither root nor the invoking identity (uid=1000); an execution object a foreign principal owns can be replaced between authorization and execution\n".into(),
            refusal("/opt/trust", foreign.clone()),
        ),
        (
            "error: verified runtime-library directory path component `/srv/shared` remains writable by Targo's effective identity (uid=1000); ACL-backed write authority is incompatible with verified pathname execution\n".into(),
            refusal("/srv/shared", Refused::Acl { me: 1000 }),
        ),
        (
            "error: the directory `/home/u/.rustup/toolchains` holding protected Trust launch path symlink `/home/u/.rustup/toolchains/trust` is group- or world-writable without the sticky bit (mode=0o775); any local principal could repoint the symlink at another toolchain\n".into(),
            refusal(
                "/home/u/.rustup/toolchains",
                Refused::Writable {
                    owner: None,
                    mode: Some(0o775),
                    link: Some("/home/u/.rustup/toolchains/trust".into()),
                },
            ),
        ),
        (
            "error: the directory `/opt/links` holding protected Trust launch path symlink `/opt/links/trust` is owned by uid=501, which is neither root nor the invoking identity (uid=1000); its owner can repoint the symlink at another toolchain\n".into(),
            refusal("/opt/links", foreign.clone()),
        ),
        (
            "error: protected Trust launch path symlink `/opt/links/trust` is owned by uid=501, which is neither root nor the invoking identity (uid=1000); a redirection a foreign principal owns can be repointed at another toolchain\n".into(),
            refusal("/opt/links/trust", foreign),
        ),
        (
            REFUSED_TRUSTD.into(),
            refusal(
                "/home/u/tc",
                Refused::Ancestor {
                    authority: "trustd executable".into(),
                },
            ),
        ),
        (
            "trustd: trustd runtime ancestor is foreign-owned or cross-user writable: /tmp\n".into(),
            refusal(
                "/tmp",
                Refused::Ancestor {
                    authority: "trustd runtime".into(),
                },
            ),
        ),
        (
            "{\"reason\":\"compiler-message\",\"message\":{\"rendered\":\"error: verified runtime-library candidate path component `/home/u/work/target-trust/j0/debug/deps/libaterm_x-1.rlib` is group- or world-writable (uid=1000, mode=0o664); any local principal could substitute the executed object between authorization and execution\\n\",\"level\":\"error\"}}\n".into(),
            refusal(
                "/home/u/work/target-trust/j0/debug/deps/libaterm_x-1.rlib",
                writable(Some(1000), 0o664),
            ),
        ),
        (
            "\x1b[1m\x1b[91merror\x1b[0m\x1b[1m:\x1b[0m verified tool launcher path component `\x1b[1m/opt/trust/bin\x1b[0m` is group- or world-writable (uid=0, mode=0o775); any local principal could substitute the executed object between authorization and execution\n".into(),
            refusal("/opt/trust/bin", writable(Some(0), 0o775)),
        ),
        (
            "error: verified runtime-library directory launch path symlink `/home/u/shared/troot` is owned by uid=501, which is not permitted to own a redirection into the verified toolchain (invoking uid=1000); its owner can repoint it at another sysroot\n".into(),
            refusal(
                "/home/u/shared/troot",
                Refused::Foreign {
                    owner: 501,
                    me: 1000,
                },
            ),
        ),
        (
            format!("targo trust: error: {TRUSTD_REFUSED} at /tmp/trustd-runtime-locks-1000/trustd.sock: socket parent must be euid-owned/non-writable or root-owned/sticky: /tmp/trustd-runtime-locks-1000; refusing to launch a split-authority Cargo fan-out.\n"),
            refusal(
                "/tmp/trustd-runtime-locks-1000",
                Refused::Demand {
                    demand: "socket parent must be euid-owned/non-writable or root-owned/sticky".into(),
                },
            ),
        ),
        (
            "targo trust: error: could not establish the private per-user trustd runtime directory: trustd runtime root must be an euid-owned 0700 directory: /tmp/trustd-runtime-locks-1000\n".into(),
            refusal(
                "/tmp/trustd-runtime-locks-1000",
                Refused::Demand {
                    demand: "trustd runtime root must be an euid-owned 0700 directory".into(),
                },
            ),
        ),
        (
            format!("targo trust: error: {TRUSTD_REFUSED} at /tmp/trustd-runtime-locks-1000/trustd.sock: could not authorize the same-sysroot trustd executable /home/u/tc/bin/trustd: trustd executable must be same-user (or immutable root-owned system), regular, executable, and have no group/other write bits: /home/u/tc/bin/trustd; refusing to launch a split-authority Cargo fan-out.\n"),
            refusal(
                "/home/u/tc/bin/trustd",
                Refused::Demand {
                    demand: "trustd executable must be same-user (or immutable root-owned system), regular, executable, and have no group/other write bits".into(),
                },
            ),
        ),
    ];
    for (text, want) in cases {
        let log = format!("{head}{text}{text}targo trust: error: verification did not run\n");
        let m = Measured::of_log(log.as_bytes(), "2");
        assert_eq!(m.path_refused, [want], "{text}");
    }
    let m = Measured::of_log(REFUSED_TRUSTD.as_bytes(), "2");
    assert_eq!(m.launch, "other");
    for text in [
        head,
        DONE,
        "error: verified tool launcher path component `/opt/trust/bin` is owned by uid=501, but TRUST_REQUIRE_SEALED_LAUNCHER demands a root-owned installation for a sealed-release provenance claim\n",
        "error: verified tool launcher launch path symlink `/home/u/tc` is owned by uid=1000, which is not permitted to own a redirection into the verified toolchain (invoking uid=1000); its owner can repoint it at another sysroot\n",
        "error: cannot inspect the directory `/x` holding protected Trust launch path symlink `/x/t`: No such file or directory (os error 2)\n",
        "error: the directory `/x` holding verified tool launcher launch path symlink `/x/t` is not a plain directory\n",
        "error: failed to inspect verified tool launcher path component `/x`\n",
    ] {
        let m = Measured::of_log(text.as_bytes(), "1");
        assert!(m.path_refused.is_empty(), "{text}: {:?}", m.path_refused);
    }
}

#[test]
fn a_refused_path_is_said_once_with_every_library_it_failed() {
    let me = aterm_uds::peer::our_uid();
    let mine = REFUSED_LOG.replace("uid=1000", &format!("uid={me}"));
    let gated = |text: &str| Decision::Gate(Measured::of_log(text.as_bytes(), "2"));
    let ratchet = format!("eps\t5\t21\t{DRV}\n");
    let v = judge(
        &facts(vec![
            ("alpha", gated(&mine)),
            ("beta", gated(&mine)),
            ("gamma", gated(REFUSED_TMP)),
            ("delta", gated(REFUSED_TRUSTD)),
            ("eps", gate(DONE)),
        ]),
        Some(&ratchet),
        None,
        "R",
        "M",
        Update::NONE,
    );
    let said: Vec<&str> = v
        .report
        .lines()
        .filter(|l| l.starts_with("REFUSED PATH:"))
        .collect();
    assert_eq!(said.len(), 3, "{}", v.report);
    let line = |libs: &str| {
        said.iter()
            .copied()
            .find(|l| l.starts_with(&format!("REFUSED PATH: {libs}: ")))
            .unwrap_or_else(|| panic!("no line for {libs}: {said:#?}"))
    };
    let both = line("alpha beta");
    assert!(
        both.contains(REFUSED_DIR)
            && both.contains("mode 775")
            && both.ends_with(&format!("run: chmod go-w {REFUSED_DIR}")),
        "{both}"
    );
    let tmp = line("gamma");
    assert!(
        tmp.contains("/tmp")
            && tmp.contains("mode 1777")
            && !tmp.contains("chmod")
            && tmp.ends_with(REFUSED_MOVE),
        "{tmp}"
    );
    let trustd = line("delta");
    assert!(
        trustd.contains("chmod go-w /home/u/tc") && trustd.ends_with(REFUSED_MOVE),
        "{trustd}"
    );
    assert_eq!(
        v.failed,
        [
            "alpha NOT-VERIFIED(rc=2)",
            "beta NOT-VERIFIED(rc=2)",
            "gamma NOT-VERIFIED(rc=2)"
        ]
    );
    assert!(
        v.report
            .lines()
            .any(|l| l.starts_with("delta ") && l.contains("NOT-VERIFIED(trustd)")),
        "{}",
        v.report
    );
    assert_eq!(v.unverified, ["delta"]);
    assert!(
        v.tail.starts_with(
            "VERDICT: FAIL — alpha NOT-VERIFIED(rc=2); beta NOT-VERIFIED(rc=2); gamma NOT-VERIFIED(rc=2)"
        ) && v.tail.contains("REFUSED PATH")
            && v.tail.contains(&format!("run: chmod go-w {REFUSED_DIR}")),
        "{}",
        v.tail
    );
    let clean = judge(
        &facts(vec![("eps", gate(DONE))]),
        Some(&ratchet),
        None,
        "R",
        "M",
        Update::NONE,
    );
    assert!(
        !clean.report.contains("REFUSED PATH") && !clean.tail.contains("REFUSED PATH"),
        "{}",
        clean.report
    );
}

#[test]
fn a_refused_paths_remedy_is_chmod_only_for_the_callers_own() {
    let me = 1000;
    let at = |why: Refused| refusal("/home/u/a b", why);
    let chmod = "chmod go-w '/home/u/a b'";
    for (said, who) in [
        (at(writable(Some(me), 0o775)).remedy(me), "the caller's"),
        (
            at(writable(Some(0), 0o775)).remedy(0),
            "root's own, to root",
        ),
    ] {
        assert!(
            said.ends_with(&format!("run: {chmod}")) && !said.contains(REFUSED_MOVE),
            "{who}: {said}"
        );
    }
    for why in [
        writable(Some(0), 0o775),
        writable(Some(0), 0o1777),
        writable(Some(me), 0o1777),
        Refused::Foreign { owner: 501, me },
        Refused::Acl { me },
    ] {
        let said = at(why.clone()).remedy(me);
        assert!(
            !said.contains("chmod") && said.ends_with(REFUSED_MOVE),
            "{why:?}: {said}"
        );
    }
    for why in [
        Refused::Writable {
            owner: None,
            mode: Some(0o775),
            link: Some("/home/u/a b/trust".into()),
        },
        Refused::Ancestor {
            authority: "trustd executable".into(),
        },
        Refused::Demand {
            demand: "trustd executable must be same-user (or immutable root-owned system), regular, executable, and have no group/other write bits".into(),
        },
    ] {
        let said = at(why.clone()).remedy(me);
        assert!(
            said.contains(&format!("run: {chmod}")) && said.ends_with(REFUSED_MOVE),
            "{why:?}: {said}"
        );
    }
    for (demand, chmod) in [
        (
            "trustd runtime root must be an euid-owned 0700 directory",
            "chmod 700 '/home/u/a b'",
        ),
        (
            "socket parent must be euid-owned/non-writable or root-owned/sticky",
            chmod,
        ),
    ] {
        let said = at(Refused::Demand {
            demand: demand.into(),
        })
        .remedy(me);
        assert!(
            said.contains(&format!("run: {chmod}")) && !said.contains(REFUSED_MOVE),
            "{demand}: {said}"
        );
    }
}

#[test]
fn a_run_targo_refused_a_path_in_files_nothing() {
    let w = World::new("refused-path");
    let a = w.prover("a");
    let head = w.commit(&[("src/lib.rs", "v1"), (RATCHET, &floor(3))]);
    let logs = w.root.join("logs");
    let slots = w.root.join("troot");
    fs::create_dir_all(slots.join("j0/debug/deps")).unwrap();
    fs::create_dir_all(&logs).unwrap();
    fs::write(
        slots.join("j0/debug/deps/alpha-1.d"),
        "alpha.rmeta: crates/alpha/src/lib.rs\n",
    )
    .unwrap();
    fs::write(logs.join(".trustd-grants"), "1\n").unwrap();
    let plan = format!(
        "{PLAN_MAGIC}\ndriver {}/targo\ndriver-id {DRV}\ntrustd /tmp/t.sock\nonly \n\
         logdir {}\nratchet {}\ncrate alpha\talpha@1.0.0\tgate\n\
         store {}\ncommit {head}\ntree x\nprover {}\nprover-bin {}\ntarget-root {}\n\
         lane-pin SOURCE_DATE_EPOCH=0\nlane-passed PATH\nlane-passed HOME\nlane-passed CARGO_TARGET_DIR\nfile \n",
        a.bin.display(),
        logs.display(),
        w.work().join(RATCHET).display(),
        w.store().display(),
        a.identity.as_ref().unwrap(),
        a.bin.display(),
        slots.display(),
    );
    let run = |log: &str, rc: &str| {
        fs::write(logs.join("alpha@1.0.0.log"), log).unwrap();
        fs::write(logs.join("alpha@1.0.0.log.rc"), rc).unwrap();
        let mut p = Plan::parse(&plan).unwrap();
        conclude(&w.cli(), &w.work(), &mut p, Update::NONE).unwrap()
    };
    let done = run(REFUSED_LOG, "2\n");
    let l = done.facts_line.unwrap_or_default();
    assert!(
        l.starts_with("FACTS: none") && l.contains(REFUSED_DIR) && l.contains("alpha"),
        "{l}"
    );
    assert!(done.verdict.report.contains("REFUSED PATH: alpha: "));
    assert!(w.found(&a, "HEAD").is_err(), "nothing was filed");
    let done = run(
        "  Summary: 3 proved, 0 failed (3 total)\nCoverage: 2/2 eligible function bodies verified (complete)\n",
        "0\n",
    );
    let l = done.facts_line.unwrap_or_default();
    assert!(l.starts_with("FACTS: ") && !l.contains("none"), "{l}");
    assert!(w.found(&a, "HEAD").is_ok(), "the control is found");
}
