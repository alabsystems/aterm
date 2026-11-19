// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn log(text: &str) -> Measured {
    Measured::of_log(text.as_bytes(), "0").unwrap()
}

#[test]
fn summary_counts_require_a_consistent_outcome_partition() {
    for (counts, proved, total) in [
        ("3 proved (7 total)", 3, 7),
        ("3 proved, 2 failed (7 total); reproducible", 3, 7),
        (
            "1 proved, 2 failed, 3 runtime-checked, 4 inconclusive (10 total)",
            1,
            10,
        ),
        (
            "1 proved, 2 failed, 3 runtime-checked, 4 assumed, 5 mandated, 6 inconclusive (21 total)",
            1,
            21,
        ),
        (
            "1 proved, 2 failed, 3 runtime-checked, 4 assumed, 5 mandated, 6 contract-panic, 7 inconclusive (28 total); measured; profile a; vector b",
            1,
            28,
        ),
    ] {
        assert_eq!(
            parse_summary(&format!("  Summary: {counts}")),
            (Some(proved), Some(total))
        );
    }
    for counts in [
        "1 proved (2 total) Summary: 5 proved (9 total)",
        "4 proved, no total",
        "4 proved, 4 failed (7 total)",
        "1 proved, 2 failed, 3 failed, 4 inconclusive (10 total)",
        "1 proved, 2 failed, 3 runtime-checked, 4 inconclusive (11 total)",
        "18446744073709551616 proved (18446744073709551616 total)",
        "18446744073709551615 proved, 1 failed (18446744073709551615 total)",
    ] {
        assert_eq!(
            parse_summary(&format!("Summary: {counts}")),
            (None, None),
            "{counts}"
        );
    }
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
    let back = Measured::from_fields(&fields.split('\t').collect::<Vec<_>>(), true);
    assert_eq!(back.map(|(b, _)| b.skipped), Some(None));
    assert!(log("targo trust: error: verification did not run\n").transport);
}

const DONE: &str = "Summary: 5 proved, 3 failed, 8 runtime-checked, 0 assumed, 0 mandated, 0 contract-panic, 5 inconclusive (21 total)\n\
                    Coverage: 9/9 eligible function bodies verified (complete)\n\
                    \x20 Gate: FAIL [advisory lane, exit 1] (5 proved / 21 total)\n";

#[test]
fn inconsistent_summary_or_coverage_cannot_raise_a_floor() {
    let report = DONE.replace(
        "Coverage: 9/9 eligible function bodies verified",
        concat!(
            "Failed sites: 3 failed row(s) at 2 distinct source site(s); 1 row(s) are hardened ",
            "panic-boundary twins of a failed overflow/bounds row at the same MIR assert (distinct ",
            "obligations: panic-freedom vs. L0 safety; both stay failed)\n",
            "Coverage: 12/12 eligible function bodies accounted for (10 verified, 2 excluded: test-harness)",
        ),
    );
    let replay = |text: &str| {
        let m = Measured::of_log(text.as_bytes(), "1").unwrap();
        let f = facts(vec![("alpha", Decision::Gate(m))]);
        let stored = Facts::parse(&f.to_record()).unwrap();
        judge(&stored, None, None, "R", "M", Update::RAISE)
    };
    let v = replay(&report);
    assert_eq!((v.word, v.total.as_str()), ("PASS", "5/21"));
    assert!(v.ratchet.unwrap().contains("alpha\t5\t21\t"));

    let wrong_summary = report.replace("5 proved,", "15 proved,");
    let wrong_coverage = report.replace("10 verified", "1 verified");
    for (broken, issue) in [
        (wrong_summary.clone(), "SUMMARY-UNPARSED"),
        (wrong_coverage, "NOT-VERIFIED(incomplete)"),
        (
            report.replace("2 excluded", "12 excluded"),
            "NOT-VERIFIED(incomplete)",
        ),
        (
            report.replace("10 verified", "18446744073709551615 verified"),
            "NOT-VERIFIED(incomplete)",
        ),
        (
            report.replace("test-harness", "unknown"),
            "NOT-VERIFIED(incomplete)",
        ),
    ] {
        let v = replay(&broken);
        assert_eq!((v.word, v.total.as_str()), ("FAIL", "0/0"), "{}", v.report);
        assert_eq!(v.failed, [format!("alpha {issue}")]);
        assert!(v.ratchet.is_none());
    }
    let v = replay(&wrong_summary.replace("10 verified", "1 verified"));
    assert!(
        v.report.contains("its Summary does not parse"),
        "{}",
        v.report
    );
    assert!(!v.report.contains("15 proved of 21"));
}

/// A complete-looking prefix followed by either a real EOF or an injected read failure.
struct LogTail<'a> {
    remaining: &'a [u8],
    fail: bool,
}

impl std::io::Read for LogTail<'_> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let available = self.fill_buf()?;
        let n = available.len().min(out.len());
        out[..n].copy_from_slice(&available[..n]);
        self.consume(n);
        Ok(n)
    }
}

impl BufRead for LogTail<'_> {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        if self.remaining.is_empty() && self.fail {
            Err(std::io::Error::other("injected log read failure"))
        } else {
            Ok(self.remaining)
        }
    }
    fn consume(&mut self, n: usize) {
        self.remaining = &self.remaining[n..];
    }
}

#[test]
fn a_complete_summary_requires_a_clean_log_eof() {
    let scratch = crate::test_scratch_dir("log-reader-errors");
    for name in ["alpha", "beta"] {
        fs::write(scratch.path().join(format!("{name}@1.0.0.log.rc")), "1\n").unwrap();
    }
    for fail in [false, true] {
        let reader = || LogTail {
            remaining: DONE.as_bytes(),
            fail,
        };
        let parsed = Measured::of_log(reader(), "1");
        if fail {
            assert_eq!(parsed.unwrap_err().to_string(), "injected log read failure");
        } else {
            let m = parsed.unwrap();
            assert_eq!(parse_summary(&m.summary), (Some(5), Some(21)));
            assert!(m.ended.is_empty());
        }
        let mut plan = Plan::parse(&format!("{PLAN_MAGIC}\nlogdir {}\ncrate alpha\talpha@1.0.0\tgate\ncrate beta\tbeta@1.0.0\tgate\n", scratch.path().display())).unwrap();
        let before = plan.facts.clone();
        let measured = plan.measure_with(|path, rc| {
            if path.ends_with("alpha@1.0.0.log") {
                Measured::of_log(DONE.as_bytes(), rc)
            } else {
                Measured::of_log(reader(), rc)
            }
        });
        let Decision::Gate(m) = &plan.facts.crates[0].decision else {
            panic!("gated")
        };
        if fail {
            assert!(
                measured
                    .unwrap_err()
                    .to_string()
                    .contains("injected log read failure")
            );
            assert!(m.summary.is_empty(), "a partial measurement is never kept");
            assert_eq!(
                plan.facts, before,
                "no earlier crate's partial facts survive"
            );
        } else {
            measured.unwrap();
            assert_eq!(parse_summary(&m.summary), (Some(5), Some(21)));
        }
    }
}

#[test]
fn present_obligation_records_must_parse_before_any_measurement_is_kept() {
    let scratch = crate::test_scratch_dir("obligation-record-errors");
    for name in ["alpha", "beta+sealed"] {
        testing::write_log(&scratch.path().join(format!("{name}@1.0.0.log")), DONE);
        fs::write(scratch.path().join(format!("{name}@1.0.0.log.rc")), "1\n").unwrap();
    }
    let valid = crate::trust_oblig::Record {
        package: "beta".into(),
        unit: r#"cargo-target(package="beta",kind=["lib"],target="beta")"#.into(),
        functions: 0,
        rows: Vec::new(),
    }
    .to_text();
    let foreign = valid.replace("beta", "gamma");
    let wrong_unit = valid.replace("package=\"beta\"", "package=\"gamma\"");
    let path = scratch.path().join("beta+sealed@1.0.0.log.oblig.tsv");
    for record in [
        None,
        Some(valid.as_str()),
        Some(foreign.as_str()),
        Some(wrong_unit.as_str()),
        Some(""),
        Some("not a record\n"),
        Some(valid.trim_end_matches("end\t0\n")),
        Some(&valid.replace("total\t0", "total\t1")),
    ] {
        if let Some(record) = record {
            fs::write(&path, record).unwrap();
        } else if path.exists() {
            fs::remove_file(&path).unwrap();
        }
        let mut plan = Plan::parse(&format!("{PLAN_MAGIC}\nlogdir {}\ncrate alpha\talpha@1.0.0\tgate\ncrate beta+sealed\tbeta+sealed@1.0.0\tgate\n", scratch.path().display())).unwrap();
        let before = plan.facts.clone();
        let measured = plan.measure();
        if record.is_none() || record == Some(valid.as_str()) {
            measured.unwrap();
            assert_eq!(
                parse_summary(&gated(&plan.facts).summary),
                (Some(5), Some(21))
            );
            let Decision::Gate(beta) = &plan.facts.crates[1].decision else {
                panic!("gated")
            };
            assert_eq!(beta.backstops.is_some(), record.is_some());
        } else {
            let error = measured.unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
            if record == Some(foreign.as_str()) || record == Some(wrong_unit.as_str()) {
                assert!(error.to_string().contains("gamma"), "{error}");
                assert!(error.to_string().contains("beta"), "{error}");
            }
            assert!(
                error
                    .to_string()
                    .contains("beta+sealed@1.0.0.log.oblig.tsv"),
                "{error}"
            );
            assert_eq!(
                plan.facts, before,
                "the earlier crate must not commit partial facts"
            );
        }
    }
}

#[test]
fn execution_status_requires_the_receipts_native_integer_format() {
    let scratch = crate::test_scratch_dir("execution-status");
    let status = scratch.path().join("compiler.log.rc");
    for code in ["0", "1", "2", "3", "101", "137", "-1073741819"] {
        fs::write(&status, format!("{code}\n")).unwrap();
        assert_eq!(execution_status(&status).unwrap(), code);
    }
    for malformed in ["", "?", "unknown", "0\n1", "+0", "01", "2147483648"] {
        fs::write(&status, malformed).unwrap();
        assert_eq!(
            execution_status(&status).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
    }
    fs::remove_file(&status).unwrap();
    assert_eq!(
        execution_status(&status).unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );
}

#[test]
fn a_present_pace_receipt_requires_a_complete_measurement() {
    let scratch = crate::test_scratch_dir("pace-receipts");
    for name in ["alpha", "beta"] {
        testing::write_log(&scratch.path().join(format!("{name}.log")), DONE);
        fs::write(scratch.path().join(format!("{name}.log.rc")), "1\n").unwrap();
    }
    let path = scratch.path().join("beta.log.pace");
    let plan = || {
        Plan::parse(&format!(
            "{PLAN_MAGIC}\nlogdir {}\ncrate alpha\talpha\tgate\ncrate beta\tbeta\tgate\n",
            scratch.path().display()
        ))
        .unwrap()
    };
    for receipt in [
        None,
        Some("20 50 - 8\n"),
        Some("20 50 400 8 2 7\r\n"),
        Some("20 50 400 8 2 7 1 10 5\n"),
    ] {
        if let Some(text) = receipt {
            fs::write(&path, text).unwrap();
        }
        let mut plan = plan();
        plan.measure().unwrap();
        let Decision::Gate(beta) = &plan.facts.crates[1].decision else {
            panic!("gated")
        };
        assert_eq!(
            beta.pace.map(|p| p.text()),
            receipt.map(|s| s.trim_end().to_string())
        );
    }
    for malformed in [
        "",
        "20 50 - 8",
        "20 50 400 8 2 7",
        "20 50 - 8 2\n",
        "unknown\n",
        "20 50 - 8\n20 50 - 8\n",
    ] {
        fs::write(&path, malformed).unwrap();
        let mut plan = plan();
        let before = plan.facts.clone();
        let error = plan.measure().unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("beta.log.pace"), "{error}");
        assert_eq!(plan.facts, before);
    }
}

#[test]
fn an_overlong_diagnostic_cannot_synthesize_summary_or_coverage_lines() {
    const OLD_CHUNK: usize = 1024 * 1024;
    let mut opaque = "x".repeat(OLD_CHUNK);
    opaque.push_str("Summary: 999 proved (999 total)\n");
    opaque.push_str(&"x".repeat(OLD_CHUNK));
    opaque.push_str("Coverage: 999/999 eligible function bodies verified (complete)\n");
    // Each token starts exactly where the old bounded reader began a continuation chunk,
    // but neither token starts a logical line in the compiler log.
    let m = Measured::of_log(opaque.as_bytes(), "0").unwrap();
    assert!(m.summary.is_empty(), "{}", m.summary);
    let mut followed = opaque.clone();
    followed.push_str(DONE);
    let m = Measured::of_log(
        std::io::BufReader::with_capacity(4093, followed.as_bytes()),
        "1",
    )
    .unwrap();
    assert_eq!(parse_summary(&m.summary), (Some(5), Some(21)));
    assert!(m.ended.is_empty(), "{}", m.ended);

    let mut no_coverage = opaque.clone();
    no_coverage.push_str("Summary: 5 proved (21 total)\n");
    let m = Measured::of_log(no_coverage.as_bytes(), "1").unwrap();
    assert!(m.ended.contains("no Coverage line"), "{}", m.ended);

    let mut tail = DONE.to_string();
    tail.push_str(&"x".repeat(OLD_CHUNK + 1));
    let clean = Measured::of_log(tail.as_bytes(), "1").unwrap();
    assert_eq!(parse_summary(&clean.summary), (Some(5), Some(21)));
    let failed = Measured::of_log(
        LogTail {
            remaining: tail.as_bytes(),
            fail: true,
        },
        "1",
    );
    assert_eq!(failed.unwrap_err().to_string(), "injected log read failure");
}

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
fn a_cargo_child_signal_comes_from_its_exit_status() {
    // Targo prints the program path verbatim, even with spaces, and shell-quotes each argument.
    let command = "  process didn't exit successfully: `/path/Application Support/probe target/observed '(signal: 15, SIGTERM: termination signal)'`";
    let measure = |status: &str| {
        let log = format!("{command} ({status})\n{DONE}");
        Measured::of_log(log.as_bytes(), "1").unwrap()
    };
    let exited = measure("exit status: 37");
    assert!(exited.ended.is_empty(), "{}", exited.ended);
    assert_eq!(exited.floor_counts(), Ok((5, 21)));
    let killed = measure("signal: 9, SIGKILL: kill");
    assert_eq!(
        killed.ended,
        "cargo: child process ended by signal 9 (SIGKILL)"
    );
    assert_eq!(
        measure("signal: 31").ended,
        "cargo: child process ended by signal 31"
    );
    let no_args = "error: process didn't exit successfully: `/tools/trustc` (signal: 15, SIGTERM: termination signal)";
    assert_eq!(
        Measured::of_log(no_args.as_bytes(), "101").unwrap().ended,
        "cargo: trustc ended by signal 15 (SIGTERM)"
    );
}

#[test]
fn a_check_that_did_not_finish_says_so_in_its_own_record() {
    let ended = |text: &str, rc: &str| Measured::of_log(text.as_bytes(), rc).unwrap().ended;
    let m = Measured::of_log(KILLED.as_bytes(), "101").unwrap();
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
    ).unwrap();
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
        !ended(&format!("{DONE}{summary}"), "0").is_empty(),
        "a second summary must supply its own coverage"
    );
    assert!(
        ended(
            &format!("{DONE}{summary}Coverage: 7/7 eligible function bodies verified (complete)\n"),
            "0"
        )
        .is_empty()
    );
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
    assert_eq!((v.word, v.exit, v.total.as_str()), ("FAIL", 1, "3/3"));
    assert_eq!(v.failed, ["alpha GATE-BROKE(rc=101)"]);
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
    let unfinished = [check("", ""), check("x", ""), check("", "60")];
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
/// error), and a refused path at exit 3, stay the lane's FAIL.
#[test]
fn targo_trusts_own_could_not_run_is_undecided_and_never_the_codes() {
    const DEFECT: &str = "  [UNKNOWN] [transport:missing-json] missing structured Trust JSON transport (targo-trust)\n\
                          COULD NOT RUN: verification did not run to a measurement: targo-trust found 1 transport defect(s) and reports each as a row of its own, which has no stable key, so the unit's verdicts cannot be compared, so this run has no counts\n\
                          Stop causes: unclassified=1 — COULD NOT RUN: resource admission refused the solver, or a row carried no stable key, so this run is not a measurement\n";
    const NO_COMPILER: &str = "targo trust: COULD NOT RUN: interrupted by SIGTERM. The run started no compiler, so it has no verdict\n";
    let ratchet = format!("alpha\t3\t3\t{DRV}\nbeta\t3\t3\t{DRV}\n");
    let judged = |log: &str, rc: &str| {
        let m = Measured::of_log(log.as_bytes(), rc).unwrap();
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
    // A non-measurement can have complete coverage and more proved rows than the floor.
    let counted = "Summary: 5 proved (7 total); not-reproducible\nCoverage: 9/9 eligible function bodies verified (complete)\n";
    for (suffix, reason) in [
        ("", None),
        (
            "Stop causes: unclassified=1 — COULD NOT RUN: resource admission refused the solver",
            Some("COULD NOT RUN: resource admission refused the solver"),
        ),
        (
            "Result: COULD NOT RUN (outcome not-reproducible: not a measurement, so its counts are no verdict; see the Stop causes line; exit 3)",
            Some("Result: COULD NOT RUN (outcome not-reproducible"),
        ),
    ] {
        let v = judged(&format!("{counted}{suffix}\n"), "3");
        if let Some(reason) = reason {
            assert!(
                note(&v, "NOT VERIFIED: alpha:").contains(reason),
                "{}",
                v.report
            );
        }
        assert_eq!(
            (v.word, v.exit, v.total.as_str()),
            ("COULD-NOT-RUN", 2, "3/3"),
            "{}",
            v.report
        );
        assert_eq!(v.unverified, ["alpha"]);
        assert!(
            v.ratchet
                .as_deref()
                .is_none_or(|r| r.contains("alpha\t3\t3\t"))
        );
    }
    let tally = "not-dispatched=2 solved=2 unclassified=2 unsupported=4";
    let result =
        "Result: COULD NOT RUN (outcome not-reproducible: see the Stop causes line; exit 3)";
    let log = format!(
        "{counted}Stop causes: {tally}; named: per-function detail — NOT REPRODUCIBLE: a stop\n{result}\n"
    );
    let v = judged(&log, "3");
    let cause = note(&v, "NOT VERIFIED: alpha:");
    assert!(cause.contains(&format!("Stop causes: {tally}")), "{cause}");
    assert!(!cause.contains("per-function detail"));
    let earlier = format!("Stop causes: {tally}\n{counted}{result}\n");
    assert!(
        !judged(&earlier, "3").report.contains(tally),
        "an earlier summary's tally does not explain this result"
    );
    let first = format!("{counted}Stop causes: unclassified=9\n{result}\n");
    let two_reports = judged(&format!("{first}{log}"), "3");
    let cause = note(&two_reports, "NOT VERIFIED: alpha:");
    assert!(
        cause.contains(tally) && !cause.contains("unclassified=9"),
        "{cause}"
    );
    let refused = judged(&format!("{first}{NO_COMPILER}{log}"), "3");
    assert!(note(&refused, "NOT VERIFIED: alpha:").contains("interrupted by SIGTERM"));
    let v = judged(counted, "2");
    assert_eq!((v.word, v.total.as_str()), ("FAIL", "3/3"));
    assert_eq!(v.failed, ["alpha GATE-BROKE(rc=2)"]);
    assert!(
        v.report
            .contains("GATED: 1 of 2 selected; not verified: alpha")
    );

    let measured = "Summary: 5 proved, 1 failed (7 total); measured; profile a; vector b\nCoverage: 9/9 eligible function bodies verified (complete)\n";
    assert_eq!(judged(measured, "1").total, "8/10");
    for outcome in ["not-reproducible", "could-not-run"] {
        let v = judged(
            &measured.replace("; measured;", &format!("; {outcome};")),
            "1",
        );
        assert_eq!((v.word, v.total.as_str()), ("FAIL", "3/3"), "{}", v.report);
        assert_eq!(v.failed, [format!("alpha NOT-MEASURED(rc=1,{outcome})")]);
        let diagnostic = note(&v, "NOT MEASURED: alpha:");
        assert!(
            diagnostic.contains("5 proved, 1 failed (7 total)"),
            "{diagnostic}"
        );
        assert!(!v.report.contains("VERDICT: 100%") && !v.tail.contains("100%"));
        assert!(
            v.ratchet
                .as_deref()
                .is_none_or(|r| r.contains("alpha\t3\t3\t"))
        );
    }
    for malformed in [
        measured.replace("5 proved", "8 proved"),
        measured.replace("; measured;", "; unrecognized;"),
        measured.replace("; measured;", " not-reproducible;"),
    ] {
        let v = judged(&malformed, "0");
        assert_eq!((v.word, v.total.as_str()), ("FAIL", "3/3"));
        assert_eq!(v.failed, ["alpha SUMMARY-UNPARSED"]);
    }
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
        testing::write_log(&dir.join(format!("{spec}.log")), DONE);
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
    p.measure().unwrap();
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
fn whole_tree_totals_preserve_an_open_obligation_past_u64_max() {
    let max = u64::MAX;
    let f = facts(vec![
        (
            "alpha",
            gate(&format!("Summary: {max} proved ({max} total)")),
        ),
        ("beta", gate("Summary: 1 proved (2 total)")),
    ]);
    let v = judge(&f, None, None, "R", "M", Update::RAISE);
    assert_eq!(v.word, "PASS");
    assert_eq!(v.total, "18446744073709551616/18446744073709551617");
    assert!(!v.tail.contains("100%"));
}

#[test]
fn a_record_round_trips_and_a_cut_short_one_is_none() {
    let mut f = facts(vec![
        ("alpha", gate("  Summary: 3 proved (3 total)")),
        ("beta", Decision::NoLib),
        ("forky", Decision::Queued("queued".into())),
    ]);
    f.ambiguous
        .push(("dup".into(), "dup@1".into(), "dup@2".into()));
    f.crates.push(Crate::new("dup", Decision::Ambiguous));
    f.provenance = Some(Provenance {
        key: "k".into(),
        ..Provenance::default()
    });
    let text = f.to_record();
    assert_eq!(Facts::parse(&text), Some(f.clone()));
    let historical = text.replacen("\ncrate ", "\nvanished ghost\ncrate ", 1);
    assert_eq!(Facts::parse(&historical), Some(f.clone()));
    let cut = text.trim_end().strip_suffix(FACTS_END).unwrap();
    assert_eq!(Facts::parse(cut), None, "no `end`, no record");
    assert_eq!(
        Facts::parse(&text.replace(FACTS_MAGIC, "aterm-trust facts 14")),
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

#[test]
fn gate_grade_evidence_survives_replay_and_v17_keeps_its_proof_floor() {
    // The v17 writer ended a Gate row at its summary. Its counted backstops followed.
    const OLD_ROW: &str = concat!(
        "crate alpha\tgate\t-\t-\t-\t-\t-\t-",
        "\t0\t0\t0\t0\t0\t0\t-",
        "\t\t\t\t\t\t\t\t\t0\t-\t1\t\t\tSummary: 5 proved (9 total)"
    );
    let record = |magic: &str, tail: &str| {
        format!("{magic}\n{OLD_ROW}{tail}\nbackstop alpha\tkey\trss-backstop\tbody\nend\n")
    };
    let legacy = Facts::parse(&record(FACTS_MAGIC_V17, "")).unwrap();
    assert_eq!(Measured::default().grade, GradeEvidence::Absent);
    assert_eq!(gated(&legacy).grade, GradeEvidence::Legacy);
    assert_eq!(
        judge(&legacy, Some(&floor(5)), None, "R", "M", Update::NONE).total,
        "5/9"
    );
    assert_eq!(Facts::parse(&legacy.to_record()), Some(legacy));
    let tally = crate::trust_credit::Tally {
        proved: 5,
        graded: 3,
        calculus: 2,
    };
    for (field, expected) in [
        ("uncollected", GradeEvidence::Legacy),
        ("absent", GradeEvidence::Absent),
        (
            "measured 0 0 0",
            GradeEvidence::Measured(Default::default()),
        ),
        ("measured 5 3 2", GradeEvidence::Measured(tally)),
    ] {
        let f = Facts::parse(&record(FACTS_MAGIC, &format!("\t{field}"))).unwrap();
        assert_eq!(gated(&f).grade, expected);
        assert_eq!(gated(&f).backstops.as_ref().unwrap()[0].key, "key");
        assert!(f.to_record().contains(&format!("{OLD_ROW}\t{field}\n")));
        assert_eq!(Facts::parse(&f.to_record()), Some(f));
    }
    assert!(Facts::parse(&record(FACTS_MAGIC_V17, "\tabsent")).is_none());
    let complete = record(FACTS_MAGIC, "\tmeasured 5 3 2");
    let (_, second_gate) = complete.split_once('\n').unwrap();
    let repeated = format!("{}{second_gate}", complete.strip_suffix("end\n").unwrap());
    assert!(Facts::parse(&repeated).is_none());
    for tail in [
        "",
        "\t",
        "\tunknown",
        "\tabsent\tabsent",
        "\tabsent extra",
        "\tmeasured",
        "\tmeasured 5 3",
        "\tmeasured 5 3 2 1",
        "\tmeasured 5  3 2",
        "\tmeasured 5 3 +2",
        "\tmeasured 05 3 2",
        "\tmeasured 5 03 2",
        "\tmeasured 5 3 02",
        "\tmeasured 18446744073709551616 0 0",
        "\tmeasured 4 5 2",
        "\tmeasured 5 2 3",
    ] {
        assert!(
            Facts::parse(&record(FACTS_MAGIC, tail)).is_none(),
            "{tail:?}"
        );
    }
    let no_library = format!("{FACTS_MAGIC}\ncrate app\tno-lib\t-\t-\t-\t-\t-\t-\tabsent\nend\n");
    assert!(Facts::parse(&no_library).is_none());
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
    fn conclude_without_grade_records(
        &self,
        plan: &mut Plan,
        update: Update<'_>,
    ) -> Result<Concluded> {
        let grades = plan
            .facts
            .crates
            .iter()
            .filter(|c| matches!(c.decision, Decision::Gate(_)))
            .map(|c| (c.name.clone(), GradeEvidence::Absent))
            .collect();
        let floors = FloorInputs::read(&plan.ratchet, &plan.memory, update)
            .map_err(|e| Error::new(format!("proof floor inputs could not be read: {e}")))?;
        conclude(&self.cli(), &self.work(), plan, &floors, &grades, None)
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

fn replay_credit(w: &World, prover: &Prover, update: bool) -> i32 {
    run_cmd(
        &w.work(),
        TrustCmd::Judge {
            commit: "HEAD".into(),
            prover: Some(prover.bin.clone()),
            update,
            reason: None,
        },
    )
}

fn measured_grade(proved: u64, calculus: u64) -> GradeEvidence {
    GradeEvidence::Measured(crate::trust_credit::Tally {
        proved,
        graded: proved,
        calculus,
    })
}

fn credit_epoch(floor: u64) -> String {
    format!("1\tproved\tunvalidated-encoder\t{floor}\tfixture calculus floor\n")
}

#[test]
fn replay_requires_the_positive_calculus_floor_present_when_facts_were_measured() {
    use crate::trust_credit::EPOCHS;
    let w = World::new("replay-credit-minimum");
    let prover = w.prover("a");
    let floors = floor(3);
    let first = credit_epoch(0);
    let measured = format!("{first}2\tunvalidated-encoder\tproven-calculus\t2\tmeasured minimum\n");
    let head = w.commit(&[(RATCHET, &floors), (EPOCHS, &measured)]);
    let mut alpha = testing::gated("alpha", "Summary: 6 proved (9 total)");
    let Decision::Gate(m) = &mut alpha.decision else {
        unreachable!()
    };
    m.grade = measured_grade(6, 3);
    let receipt = testing::file(&w.cli(), &w.store(), &prover, &head, DRV, vec![alpha]);
    let original = fs::read(&receipt).unwrap();
    let path = w.work().join(EPOCHS);
    // Three calculus proofs meet both numeric floors. The policy change itself must fail.
    for truncated in [None, Some(first)] {
        match truncated {
            None => fs::remove_file(&path).unwrap(),
            Some(text) => fs::write(&path, text).unwrap(),
        }
        assert_eq!(replay_credit(&w, &prover, true), 1);
        assert_eq!(fs::read_to_string(w.work().join(RATCHET)).unwrap(), floors);
        assert!(!w.work().join(BACKSTOPS).exists());
        assert_eq!(fs::read(&receipt).unwrap(), original);
    }
    fs::write(
        path,
        format!("{measured}3\tproven-calculus\tproven-calculus\t3\tstronger policy\n"),
    )
    .unwrap();
    assert_eq!(replay_credit(&w, &prover, false), 0);
}

#[test]
fn a_malformed_historical_credit_ledger_names_its_source_and_blocks_updates() {
    use crate::trust_credit::EPOCHS;
    let w = World::new("replay-malformed-credit");
    let prover = w.prover("a");
    let before = floor(3);
    let head = w.commit(&[(RATCHET, &before), (EPOCHS, "bad historical ledger\n")]);
    let receipt = w.file(&head, &prover, 6, 9);
    let recorded = fs::read(&receipt).unwrap();
    fs::write(w.work().join(EPOCHS), credit_epoch(0)).unwrap();
    let why = working_credit_epoch(&w.cli(), &w.work(), Some(&head)).unwrap_err();
    assert!(
        why.contains("CREDIT-UNREADABLE") && why.contains(&head) && why.contains(EPOCHS),
        "{why}"
    );
    assert_eq!(replay_credit(&w, &prover, true), 1);
    assert_eq!(fs::read_to_string(w.work().join(RATCHET)).unwrap(), before);
    assert_eq!(fs::read(receipt).unwrap(), recorded);
}

#[test]
fn historical_credit_requires_a_regular_utf8_blob() {
    use crate::trust_credit::EPOCHS;
    let w = World::new("replay-credit-file");
    let path = w.work().join(EPOCHS);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    // The reason is free text. Lossy decoding would turn this into a valid ledger.
    fs::write(&path, b"1\tproved\tunvalidated-encoder\t1\twhy \xff\n").unwrap();
    w.git(&["add", "--", EPOCHS]);
    let invalid = w.commit(&[]);
    fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink("credit-policy", &path).unwrap();
    w.git(&["add", "--", EPOCHS]);
    let linked = w.commit(&[]);
    fs::remove_file(&path).unwrap();
    fs::write(&path, credit_epoch(1)).unwrap();
    for (head, defect) in [(invalid, "not UTF-8"), (linked, "regular Git blob")] {
        let why = working_credit_epoch(&w.cli(), &w.work(), Some(&head)).unwrap_err();
        assert!(
            why.contains("CREDIT-UNREADABLE")
                && why.contains(&head)
                && why.contains(EPOCHS)
                && why.contains(defect),
            "{why}"
        );
    }
}

#[test]
fn original_policy_reads_a_large_sha256_blob() {
    let dir = crate::test_scratch_dir("sha256-policy");
    let repo = dir.path();
    let git = Git::new(repo);
    crate::ledger::git_ok(&git, &["init", "--quiet", "--object-format=sha256"]).unwrap();
    let path = repo.join(BACKSTOPS);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let body = "#".repeat(aterm_uds::deadline::OUTPUT_KEEP + 1);
    fs::write(&path, &body).unwrap();
    crate::ledger::git_ok(&git, &["add", "--", BACKSTOPS]).unwrap();
    let tree = crate::ledger::git_ok(&git, &["write-tree"])
        .unwrap()
        .stdout_utf8();
    let tree = tree.trim();
    assert!(aterm_codec::hex::is_lower(tree, 64));
    assert!(floors_at(&git, tree, BACKSTOPS).unwrap().as_deref() == Some(body.as_str()));
}

#[test]
fn replay_credit_judges_the_working_epoch_before_writing_any_floor() {
    use crate::trust_credit::EPOCHS;
    let w = World::new("replay-credit");
    let prover = w.prover("a");
    let proof_floor = floor(3);
    let backstops = "alpha\t-\tcancelled\t1\n";
    let memory = "alpha\t16.0\t-\t-\t-\t-\t-\n";
    let head = w.commit(&[
        (RATCHET, &proof_floor),
        (BACKSTOPS, backstops),
        (MEMORY, memory),
        (EPOCHS, &credit_epoch(0)),
    ]);
    let mut alpha = testing::gated("alpha", "Summary: 6 proved (9 total)");
    let Decision::Gate(m) = &mut alpha.decision else {
        unreachable!()
    };
    m.grade = measured_grade(6, 2);
    m.ceiling = "8.0 GiB".into();
    let receipt = testing::file(&w.cli(), &w.store(), &prover, &head, DRV, vec![alpha]);
    let original = fs::read(&receipt).unwrap();
    let epoch = w.work().join(EPOCHS);
    for ledger in [credit_epoch(3), "invalid ledger\n".into()] {
        fs::write(&epoch, &ledger).unwrap();
        for update in [false, true] {
            assert_eq!(
                replay_credit(&w, &prover, update),
                1,
                "{ledger}, update={update}"
            );
            for (path, before) in [
                (RATCHET, proof_floor.as_str()),
                (BACKSTOPS, backstops),
                (MEMORY, memory),
            ] {
                assert_eq!(
                    fs::read_to_string(w.work().join(path)).unwrap(),
                    before,
                    "{path}"
                );
            }
            assert_eq!(fs::read(&receipt).unwrap(), original);
        }
    }
    fs::write(epoch, credit_epoch(2)).unwrap();
    assert_eq!(replay_credit(&w, &prover, true), 0);
    assert_eq!(
        fs::read_to_string(w.work().join(RATCHET)).unwrap(),
        floor(6)
    );
    assert_ne!(
        fs::read_to_string(w.work().join(BACKSTOPS)).unwrap(),
        backstops
    );
    assert_ne!(fs::read_to_string(w.work().join(MEMORY)).unwrap(), memory);
    assert_eq!(fs::read(receipt).unwrap(), original);
}

#[test]
fn replay_credit_requires_a_remeasurement_when_a_gated_grade_was_uncollected() {
    use crate::trust_credit::EPOCHS;
    for v17 in [true, false] {
        let w = World::new("replay-uncollected-grades");
        let prover = w.prover("a");
        let baseline = floor(3) + &format!("beta\t3\t9\t{DRV}\n");
        let head = w.commit(&[(RATCHET, &baseline), (EPOCHS, &credit_epoch(0))]);
        let mut alpha = testing::gated("alpha", "Summary: 5 proved (9 total)");
        let Decision::Gate(m) = &mut alpha.decision else {
            unreachable!()
        };
        m.grade = GradeEvidence::Legacy;
        let mut beta = testing::gated("beta", "Summary: 5 proved (9 total)");
        let Decision::Gate(m) = &mut beta.decision else {
            unreachable!()
        };
        m.grade = if v17 {
            GradeEvidence::Legacy
        } else {
            measured_grade(5, 2)
        };
        let receipt = testing::file(&w.cli(), &w.store(), &prover, &head, DRV, vec![alpha, beta]);
        if v17 {
            let older = fs::read_to_string(&receipt)
                .unwrap()
                .replacen(FACTS_MAGIC, FACTS_MAGIC_V17, 1)
                .replace("\tuncollected\n", "\n");
            fs::write(&receipt, older).unwrap();
        }
        let original = fs::read(&receipt).unwrap();
        let epoch = w.work().join(EPOCHS);
        // Even a known subset meeting the positive floor cannot establish the full count.
        fs::write(&epoch, credit_epoch(1)).unwrap();
        assert_eq!(replay_credit(&w, &prover, true), 2);
        assert_eq!(
            fs::read_to_string(w.work().join(RATCHET)).unwrap(),
            baseline
        );
        assert!(!w.work().join(BACKSTOPS).exists());
        for ledger in [Some(credit_epoch(0)), None] {
            if let Some(ledger) = ledger {
                fs::write(&epoch, ledger).unwrap();
            } else {
                fs::remove_file(&epoch).unwrap();
            }
            assert_eq!(replay_credit(&w, &prover, false), 0);
        }
        fs::write(
            w.work().join(RATCHET),
            baseline.replace(&floor(3), &floor(6)),
        )
        .unwrap();
        assert_eq!(replay_credit(&w, &prover, false), 1);
        assert_eq!(fs::read(receipt).unwrap(), original);
    }
}

#[test]
fn replay_refuses_duplicate_working_and_measured_floors_before_updates() {
    let duplicate = floor(3) + &floor(5);
    for (historical, proved) in [(false, 3), (false, 4), (true, 6)] {
        let w = World::new("replay-duplicate-floors");
        let prover = w.prover("a");
        let (original, working) = if historical {
            (duplicate.clone(), floor(5))
        } else {
            (floor(3), duplicate.clone())
        };
        let head = w.commit(&[("src/lib.rs", "v1"), (RATCHET, &original)]);
        w.file(&head, &prover, proved, 9);
        fs::write(w.work().join(RATCHET), &working).unwrap();
        let (facts, _) = w.found(&prover, &head).unwrap();
        let why = lowered_since_measured(&w.cli(), &facts, Some(&working)).unwrap_err();
        assert!(
            why.contains(RATCHET) && why.contains("duplicate floor for alpha"),
            "{why}"
        );
        if historical {
            assert!(why.contains(&head), "{why}");
        }
        for update in [false, true] {
            let cmd = TrustCmd::Judge {
                commit: head.clone(),
                prover: Some(prover.bin.clone()),
                update,
                reason: None,
            };
            assert_eq!(
                run_cmd(&w.work(), cmd),
                2,
                "historical={historical}, update={update}"
            );
            assert_eq!(fs::read_to_string(w.work().join(RATCHET)).unwrap(), working);
            assert!(!w.work().join(BACKSTOPS).exists());
        }
    }
}

#[test]
fn replay_refuses_invalid_floor_counts_before_updates() {
    for (historical, total) in [(false, "oops"), (false, "4"), (true, "oops"), (true, "4")] {
        let w = World::new("replay-invalid-floor-counts");
        let prover = w.prover("a");
        let invalid = format!("alpha\t5\t{total}\t{DRV}\n");
        let valid = floor(5);
        let (baseline, working) = if historical {
            (&invalid, &valid)
        } else {
            (&valid, &invalid)
        };
        let head = w.commit(&[("src/lib.rs", "v1"), (RATCHET, baseline)]);
        let receipt = w.file(&head, &prover, 6, 9);
        let recorded = fs::read(&receipt).unwrap();
        fs::write(w.work().join(RATCHET), working).unwrap();
        for update in [false, true] {
            let cmd = TrustCmd::Judge {
                commit: head.clone(),
                prover: Some(prover.bin.clone()),
                update,
                reason: None,
            };
            assert_eq!(
                run_cmd(&w.work(), cmd),
                2,
                "historical={historical}, total={total}, update={update}"
            );
            assert_eq!(
                fs::read_to_string(w.work().join(RATCHET)).unwrap(),
                *working
            );
            assert!(!w.work().join(BACKSTOPS).exists());
            assert_eq!(fs::read(&receipt).unwrap(), recorded);
        }
    }
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
fn earlier_input_keys_require_a_new_whole_tree_run() {
    let w = World::new("old-dep-info-key");
    let prover = w.prover("a");
    let head = w.commit(&[("src/lib.rs", "v1"), (RATCHET, &floor(5))]);
    let current_path = w.file(&head, &prover, 5, 9);
    let (mut historical, _) = w.found(&prover, &head).unwrap();
    fs::remove_file(&current_path).unwrap();
    let pending = || pending_for(&w.cli(), &prover, &head, false).unwrap();
    let mut archived = Vec::new();
    for epoch in [5, 6, 7] {
        let p = historical.provenance.as_mut().unwrap();
        let old_material = format!(
            "aterm-trust facts key {epoch}\ntree-sans-floors {}\nprover {}\nlane {}\n",
            p.tree_sans_floors, p.prover, p.lane
        );
        p.key = hex(&Sha256::digest(old_material.as_bytes()));
        let old_path = w.store().join(format!("{FACTS_PREFIX}{}", p.key));
        for summary in [
            "Summary: 5 proved (9 total)",
            "Summary: 0 proved (9 total); not-reproducible",
        ] {
            historical.crates[0].decision = gate(summary);
            file_record(&w.store(), &historical).unwrap();
            let why = w.found(&prover, &head).unwrap_err();
            assert!(why.contains("ship trust run --whole-tree"), "{why}");
            assert!(pending().is_err());
            assert_eq!(replay_credit(&w, &prover, false), 1);
            assert_eq!(
                fs::read_to_string(&old_path).unwrap(),
                historical.to_record()
            );
        }
        archived.push((old_path, historical.to_record()));
    }
    assert_eq!(w.file(&head, &prover, 5, 9), current_path);
    assert_eq!(replay_credit(&w, &prover, false), 0);
    assert!(pending().unwrap().is_empty());
    for (path, original) in archived {
        assert_eq!(fs::read_to_string(path).unwrap(), original);
    }
}

#[test]
fn version_15_facts_require_a_new_whole_tree_measurement() {
    let w = World::new("old-facts");
    let prover = w.prover("a");
    let head = w.commit(&[("src/lib.rs", "v1"), (RATCHET, &floor(5))]);
    let path = w.file(&head, &prover, 5, 9);
    assert!(w.found(&prover, "HEAD").is_ok());
    let old_schema = "aterm-trust facts 16";
    let old = fs::read_to_string(&path)
        .unwrap()
        .replacen(FACTS_MAGIC, old_schema, 1);
    fs::write(&path, old).unwrap();
    let why = w.found(&prover, "HEAD").unwrap_err();
    assert!(
        why.contains(old_schema) && why.contains(FACTS_MAGIC),
        "{why}"
    );
    assert!(
        why.contains("targo --unverified ship trust run --whole-tree"),
        "{why}"
    );
}

#[test]
fn replay_compares_each_crate_with_its_latest_measurement_commit() {
    let w = World::new("remeasured-floor-origins");
    let prover = w.prover("a");
    let rows = |alpha, beta| format!("alpha\t{alpha}\t9\t{DRV}\nbeta\t{beta}\t9\t{DRV}\n");
    let alien = w.commit(&[
        ("src/lib.rs", "another source tree"),
        (RATCHET, &rows(2, 2)),
    ]);
    // Only beta uses this commit. Both alpha rows, including the bad total, were superseded.
    let original = w.commit(&[
        ("src/lib.rs", "v1"),
        (RATCHET, &(rows(3, 3) + "alpha\t4\tbad\n")),
    ]);
    let first = w.commit(&[(RATCHET, &rows(8, 8))]);
    let last = w.commit(&[(RATCHET, &rows(6, 8))]);
    let crate_row = |name| testing::gated(name, "Summary: 7 proved (9 total)");
    testing::file(
        &w.cli(),
        &w.store(),
        &prover,
        &original,
        DRV,
        vec![crate_row("alpha"), crate_row("beta")],
    );
    let (mut f, _) = w.found(&prover, "HEAD").unwrap();
    let missing = "1111111111111111111111111111111111111111";
    for (head, names) in [
        (first.as_str(), vec!["alpha"]),
        (last.as_str(), vec!["alpha"]),
        (missing, vec![]),
    ] {
        let mut again = f.clone();
        let p = again.provenance.as_mut().unwrap();
        p.head = head.into();
        p.date = "2026-11-17T20:30:00Z".into();
        again.crates = names.into_iter().map(crate_row).collect();
        f = remeasured(&f, &again).unwrap();
    }
    let rounds = &mut f.provenance.as_mut().unwrap().remeasured;
    assert_eq!(rounds.len(), 2, "a new empty round adds no measurement");
    rounds.insert(0, format!("2026-11-17T10:10:00Z {missing} alpha"));
    rounds.push(format!("2026-11-17T21:00:00Z {missing} nothing"));
    file_record(&w.store(), &f).unwrap();
    let replay = || {
        run_cmd(
            &w.work(),
            TrustCmd::Judge {
                commit: "HEAD".into(),
                prover: Some(prover.bin.clone()),
                update: false,
                reason: None,
            },
        )
    };
    fs::write(w.work().join(RATCHET), rows(6, 3)).unwrap();
    assert_eq!(
        replay(),
        0,
        "beta kept its original measurement; the no-op commit is irrelevant"
    );
    fs::write(w.work().join(RATCHET), rows(4, 3)).unwrap();
    assert_eq!(replay(), 1, "alpha was last measured with a floor of six");
    for bad_round in [
        "2026-11-17T20:30:00Z",
        "2026-11-17T20:30:00Z HEAD alpha",
        &format!("2026-11-17T20:30:00Z {last} gamma"),
        &format!("2026-11-17T20:30:00Z {alien} alpha"),
        &format!("2026-11-17T20:30:00Z {missing} alpha"),
    ] {
        let mut bad = f.clone();
        bad.provenance
            .as_mut()
            .unwrap()
            .remeasured
            .push(bad_round.into());
        file_record(&w.store(), &bad).unwrap();
        assert_eq!(replay(), 2, "{bad_round}");
    }
}

#[test]
fn remeasurement_history_distinguishes_a_crate_named_nothing_from_no_work() {
    let original = "1111111111111111111111111111111111111111";
    let next = "2222222222222222222222222222222222222222";
    let mut base = facts(vec![
        ("nothing", gate("Summary: 1 proved (1 total)")),
        ("twin", Decision::Ambiguous),
        ("twin", Decision::Ambiguous),
    ]);
    base.provenance = Some(Provenance {
        head: original.into(),
        date: "2026-11-17T10:00:00Z".into(),
        ..Provenance::default()
    });
    let mut again = base.clone();
    again.crates.retain(|c| c.name == "nothing");
    again.provenance.as_mut().unwrap().head = next.into();
    let merged = remeasured(&base, &again).unwrap();
    let stored = Facts::parse(&merged.to_record()).unwrap();
    let p = stored.provenance.as_ref().unwrap();
    assert_eq!(measurement_heads(&stored, p).unwrap()["nothing"], next);
    let mut legacy = p.clone();
    legacy.remeasured = vec![format!("{} {next} nothing", p.date)];
    assert!(measurement_heads(&stored, &legacy).is_err());
}

#[test]
fn remeasurement_retries_stopped_checks_but_keeps_completed_failures() {
    let w = World::new("retry-incomplete");
    let prover = w.prover("a");
    let head = w.commit(&[("src/lib.rs", "pub fn checked() {}"), (RATCHET, &format!(
        "budget\t1\t1\t{DRV}\nsignalled\t1\t1\t{DRV}\ntransport\t1\t1\t{DRV}\nlegacy\t0\t0\t{DRV}\nregressed\t5\t9\t{DRV}\n"
    ))]);
    let stopped = |name, ended: &str, budget: &str, transport| {
        Crate::new(
            name,
            Decision::Gate(Measured {
                rc: "143".into(),
                ended: ended.into(),
                wall_budget: budget.into(),
                transport,
                ..Measured::default()
            }),
        )
    };
    let legacy = Crate::new(
        "legacy",
        Decision::Gate(Measured {
            summary: "Summary: 0 proved (0 total)".into(),
            grade: GradeEvidence::Legacy,
            ..Measured::default()
        }),
    );
    let reported = |name, rc, summary| {
        let log =
            format!("{summary}\nCoverage: 56/56 eligible function bodies verified (complete)\n");
        Crate::new(
            name,
            Decision::Gate(Measured::of_log(log.as_bytes(), rc).unwrap()),
        )
    };
    let no_measurement = "Summary: 6 proved, 4 failed (63 total); not-reproducible";
    let rows = vec![
        legacy,
        reported("clock", "1", no_measurement),
        reported("driver", "0", "Summary: 0 proved (0 total); could-not-run"),
        reported("broken", "2", no_measurement),
        reported(
            "malformed",
            "1",
            "Summary: 6 proved, 4 failed (3 total); not-reproducible",
        ),
        stopped("budget", "", "60", false),
        stopped("signalled", "trustc ended by signal 15", "", false),
        stopped("transport", "", "", true),
        testing::gated("regressed", "Summary: 2 proved (9 total)"),
    ];
    testing::file(&w.cli(), &w.store(), &prover, &head, DRV, rows);
    let verdict = w.judged(&prover);
    assert_eq!((verdict.exit, verdict.total.as_str()), (1, "2/9"));
    assert!(
        verdict
            .failed
            .iter()
            .any(|why| why.starts_with("regressed "))
    );
    let mut pending = pending_for(&w.cli(), &prover, "HEAD", false)
        .unwrap()
        .unwrap();
    pending.sort();
    assert_eq!(
        pending,
        [
            "budget",
            "clock",
            "driver",
            "legacy",
            "signalled",
            "transport"
        ]
    );
}

#[test]
fn the_fences_still_fire_from_stored_facts() {
    let w = World::new("fences");
    let a = w.prover("a");
    let head = w.commit(&[("src/lib.rs", "v1"), (RATCHET, &floor(5))]);
    w.file(&head, &a, 9, 9);
    let all_proved = w.judged(&a);
    assert!(all_proved.tail.starts_with("VERDICT: 100% MACHINE-PROVED"));
    // The report is printed even if the replay rejects its working floor. Only
    // the final tail can make the whole-tree claim after that check succeeds.
    fs::write(w.work().join(RATCHET), floor(4)).unwrap();
    assert!(!all_proved.report.contains("VERDICT: 100%"));
    assert_eq!(
        run_cmd(
            &w.work(),
            TrustCmd::Judge {
                commit: head.clone(),
                prover: Some(a.bin.clone()),
                update: false,
                reason: None,
            },
        ),
        1
    );
    fs::write(w.work().join(RATCHET), floor(5)).unwrap();
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
    testing::write_log(
        &logs.join("alpha@1.0.0.log"),
        "  Summary: 3 proved, 0 failed (3 total)\nCoverage: 2/2 eligible function bodies verified (complete)\n",
    );
    fs::write(logs.join("alpha@1.0.0.log.rc"), "0\n").unwrap();
    fs::write(logs.join(".trustd-grants"), "1\n").unwrap();
    let dep = slots.join("j0/debug/deps/alpha-0123456789abcdef.d");
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
        w.conclude_without_grade_records(&mut p, Update::NONE)
            .unwrap()
    };
    let facts_line = |extra: &str| run(extra).facts_line.unwrap_or_default();
    assert!(facts_line("file the checkout is not clean\n").starts_with("FACTS: none"));
    assert!(
        run("").facts_line.is_none(),
        "a run that does not file says nothing"
    );
    let mut bare = Plan::parse(&plan("file \n").replace(LANE, "")).unwrap();
    let l = w
        .conclude_without_grade_records(&mut bare, Update::NONE)
        .unwrap()
        .facts_line
        .unwrap();
    assert!(l.starts_with("FACTS: none"), "{l}");
    for excluded in [RATCHET, MEMORY, COSTS, BACKSTOPS] {
        fs::write(
            &dep,
            format!("alpha.rmeta: crates/alpha/src/lib.rs {excluded}\n"),
        )
        .unwrap();
        let line = facts_line("file \n");
        assert!(
            line.starts_with("FACTS: none") && line.contains(excluded),
            "{line}"
        );
    }
    let reads = |lines: &str| {
        fs::write(&dep, format!("{plain}\n{lines}")).unwrap();
        facts_line("file \n")
    };
    for lines in [
        "# env-dep:ATERM_RELEASE_BUILD=1\n",
        "# env-dep:HOME\n",
        "# env-dep:LANG=en_US.UTF-8\n# env-dep:HOME=/Users//x\n",
        "# env-dep:SOURCE_DATE_EPOCH=5\n",
        "# env-dep:SOURCE_DATE_EPOCH\n",
        "# env-dep:ATERM_GIT_COMMIT=trust-lane\n",
        "# env-dep:CARGO_PKG_BUILD_LABEL=unbound\n",
        "# env-dep:CARGO_MANIFEST_LABEL=unbound\n",
    ] {
        let l = reads(lines);
        assert!(
            l.starts_with("FACTS: none")
                && !l.contains("/Users//x")
                && !l.contains("clean checkout"),
            "{lines}: {l}"
        );
    }
    fs::write(&dep, plain).unwrap();
    let extra = slots.join("j0/debug/deps/beta-2.d");
    fs::File::create(&extra)
        .unwrap()
        .set_len(crate::durable::MAX_RELEASE_FILE_BYTES + 1)
        .unwrap();
    let incomplete = run("file \n");
    assert_eq!(incomplete.verdict.word, "PASS");
    let line = incomplete.facts_line.unwrap();
    assert!(
        line.starts_with("FACTS: none") && line.contains("beta-2.d"),
        "{line}"
    );
    assert!(w.found(&a, "HEAD").is_err());
    fs::write(&extra, b"beta.rmeta: beta.rs\n# env-dep:LABEL=\xff\n").unwrap();
    let line = facts_line("file \n");
    assert!(
        line.starts_with("FACTS: none") && line.contains("beta-2.d"),
        "{line}"
    );
    fs::remove_file(&extra).unwrap();
    let malformed_slot = slots.join("j9");
    fs::write(&malformed_slot, "").unwrap();
    let line = facts_line("file \n");
    assert!(
        line.starts_with("FACTS: none") && line.contains("j9"),
        "{line}"
    );
    fs::remove_file(malformed_slot).unwrap();
    let build = slots.join("j0/debug/build/aterm-gui-0123/output");
    fs::create_dir_all(build.parent().unwrap()).unwrap();
    fs::write(
        &build,
        "cargo:rerun-if-env-changed=ATERM_BUILD_GIT_COMMIT\ncargo:rustc-env=ATERM_GIT_COMMIT=trust-lane\n",
    )
    .unwrap();
    let unrelated = reads("# env-dep:ATERM_GIT_COMMIT=trust-lane\n");
    assert!(unrelated.starts_with("FACTS: none"), "{unrelated}");
    assert!(w.found(&a, "HEAD").is_err());
    for env in ["CARGO_PKG_VERSION=1.0.0", "SOURCE_DATE_EPOCH=0"] {
        assert!(reads(&format!("# env-dep:{env}\n")).starts_with("FACTS: none"));
    }
    let consumer = slots.join("j0/debug/.fingerprint/alpha-0123456789abcdef");
    fs::create_dir_all(&consumer).unwrap();
    fs::write(consumer.join("lib-alpha.json"), r#"{"deps":[]}"#).unwrap();
    assert!(reads("# env-dep:OUT_DIR=/x/out\n").starts_with("FACTS: none"));
    let l = reads(
        "# env-dep:ATERM_RELEASE_BUILD\n# env-dep:CARGO_PKG_VERSION=1.0.0\n# env-dep:TARGO_PKG_VERSION=1.0.0\n\
         # env-dep:SOURCE_DATE_EPOCH=0\n",
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
    let done = w
        .conclude_without_grade_records(&mut unlisted, Update::NONE)
        .unwrap();
    let l = done.facts_line.unwrap();
    assert!(
        l.starts_with("FACTS: none") && l.contains("git could not list"),
        "{l}"
    );
    assert_eq!(done.verdict.word, "PASS");
    let mut moved =
        Plan::parse(&plan("file \n").replace(a.identity.as_ref().unwrap(), "0000")).unwrap();
    let l = w
        .conclude_without_grade_records(&mut moved, Update::NONE)
        .unwrap()
        .facts_line
        .unwrap();
    assert!(l.starts_with("FACTS: none"), "{l}");
    let done = run("");
    assert_eq!(done.verdict.word, "PASS");
}

#[test]
fn an_unreadable_log_prevents_conclusion_and_ratchet_changes() {
    let w = World::new("unreadable-log");
    w.commit(&[(RATCHET, &floor(3))]);
    let logs = w.root.join("logs");
    fs::create_dir_all(logs.join("alpha@1.0.0.log.zst")).unwrap();
    fs::write(logs.join("alpha@1.0.0.log.rc"), "0\n").unwrap();
    let ratchet = fs::read(w.work().join(RATCHET)).unwrap();
    for update in [Update::NONE, Update::RAISE] {
        let mut plan = Plan::parse(&format!(
            "{PLAN_MAGIC}\nlogdir {}\nratchet {}\nstore {}\nfile \ncrate alpha\talpha@1.0.0\tgate\n",
            logs.display(), w.work().join(RATCHET).display(), w.store().display(),
        )).unwrap();
        let error = w
            .conclude_without_grade_records(&mut plan, update)
            .unwrap_err();
        assert!(
            error
                .0
                .contains("compiler execution receipts could not be measured")
                && error.0.contains("alpha@1.0.0.log"),
            "{error}"
        );
        assert_eq!(fs::read(w.work().join(RATCHET)).unwrap(), ratchet);
        assert!(!w.store().exists(), "no facts were filed");
    }
}

#[test]
fn floor_inputs_preserve_text_and_true_absence() {
    let scratch = crate::test_scratch_dir("optional-floor-inputs");
    let ratchet = scratch.path().join("floors.tsv");
    let memory = scratch.path().join("memory.tsv");
    let missing = FloorInputs::read(&ratchet, &memory, Update::NONE).unwrap();
    assert_eq!((missing.ratchet, missing.backstops), (None, None));
    let floor = "\n# retained formatting\nalpha\t3\t9\told prover\t\n";
    fs::write(&ratchet, floor).unwrap();
    let inputs = FloorInputs::read(&ratchet, &memory, Update::NONE).unwrap();
    assert_eq!(inputs.ratchet.as_deref(), Some(floor));
    assert_eq!(inputs.backstops, None);
    let history = "# a legacy floor may have no head row\n";
    fs::write(backstops_beside(&ratchet), history).unwrap();
    let inputs = FloorInputs::read(&ratchet, &memory, Update::NONE).unwrap();
    assert_eq!(inputs.backstops.as_deref(), Some(history));
    assert_eq!(floor_run(inputs.backstops.as_deref(), "alpha"), None);
}

#[test]
fn a_live_floor_raise_serializes_a_weaker_stored_judgment() {
    let w = World::new("floor-writers");
    let prover = w.prover("a");
    let head = w.commit(&[("src/lib.rs", ""), (RATCHET, &floor(5)), (MEMORY, "")]);
    w.file(&head, &prover, 6, 9);
    let replay = |update| {
        run_cmd(
            &w.work(),
            TrustCmd::Judge {
                commit: head.clone(),
                prover: Some(prover.bin.clone()),
                update,
                reason: None,
            },
        )
    };
    assert_eq!(
        replay(false),
        0,
        "the weaker receipt met the original floor"
    );

    let ratchet = w.work().join(RATCHET);
    let memory = w.work().join(MEMORY);
    let logs = w.root.join("logs");
    fs::create_dir(&logs).unwrap();
    testing::write_log(
        &logs.join("alpha@1.0.0.log"),
        "Summary: 9 proved (9 total)\nCoverage: 9/9 eligible function bodies verified (complete)\n",
    );
    fs::write(logs.join("alpha@1.0.0.log.rc"), "0\n").unwrap();
    fs::write(logs.join(".trustd-grants"), "1\n").unwrap();
    let mut plan = Plan::parse(&format!(
        "{PLAN_MAGIC}\ndriver-id {DRV}\nlogdir {}\nratchet {}\nmemory-file {}\ncrate alpha\talpha@1.0.0\tgate\n",
        logs.display(), ratchet.display(), memory.display(),
    )).unwrap();
    std::thread::scope(|threads| {
        let floors = FloorInputs::read(&ratchet, &memory, Update::RAISE).unwrap();
        let second = threads.spawn(|| replay(true));
        let probe = fs::File::open(ratchet.parent().unwrap()).unwrap();
        let attempt = aterm_uds::file_lock::lock_exclusive_until(
            &probe,
            std::time::Instant::now(),
            "another writer holds this checkout",
        );
        if attempt.is_ok() {
            probe.unlock().unwrap();
        }
        assert_eq!(attempt.unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
        let grades = BTreeMap::from([("alpha".into(), GradeEvidence::Absent)]);
        let done = conclude(&w.cli(), &w.work(), &mut plan, &floors, &grades, None).unwrap();
        assert_eq!(done.verdict.word, "PASS", "{}", done.verdict.report);
        drop(floors);
        assert_eq!(
            second.join().unwrap(),
            1,
            "the replay judged the raised floor"
        );
    });
    assert_eq!(fs::read_to_string(&ratchet).unwrap(), floor(9));
    assert!(
        floor_run(
            Some(&fs::read_to_string(w.work().join(BACKSTOPS)).unwrap()),
            "alpha"
        )
        .is_some()
    );
}

#[test]
fn an_equal_floor_replay_keeps_the_working_memory_ceiling() {
    let w = World::new("floor-memory-replay");
    let prover = w.prover("a");
    let original = "alpha\t16.0\t-\t-\t-\t-\t-\n";
    let current = "alpha\t4.0\t-\t-\t-\t-\t-\n";
    let head = w.commit(&[("src/lib.rs", ""), (RATCHET, &floor(5)), (MEMORY, original)]);
    let mut alpha = testing::gated("alpha", "Summary: 5 proved (9 total)");
    alpha.memory.f = "16.0".into();
    let Decision::Gate(measured) = &mut alpha.decision else {
        unreachable!()
    };
    measured.ceiling = "8.0 GiB".into();
    let receipt = testing::file(&w.cli(), &w.store(), &prover, &head, DRV, vec![alpha]);
    let before = fs::read(&receipt).unwrap();
    let memory = w.work().join(MEMORY);
    fs::write(&memory, current).unwrap();
    assert_eq!(
        run_cmd(
            &w.work(),
            TrustCmd::Judge {
                commit: head,
                prover: Some(prover.bin),
                update: true,
                reason: None
            }
        ),
        0
    );
    assert_eq!(fs::read_to_string(memory).unwrap(), current);
    assert_eq!(
        fs::read(receipt).unwrap(),
        before,
        "admission evidence stays as measured"
    );
}

#[test]
fn unreadable_floor_inputs_prevent_measurement_and_publication() {
    for update in [Update::NONE, Update::RAISE] {
        for target in [RATCHET, BACKSTOPS] {
            for directory in [false, true] {
                let w = World::new("unreadable-floor-inputs");
                w.commit(&[
                    (RATCHET, &format!("alpha\t3\t21\t{DRV}\n")),
                    (BACKSTOPS, "alpha\t-\tcancelled\t0\n"),
                ]);
                let logs = w.root.join("logs");
                fs::create_dir(&logs).unwrap();
                testing::write_log(&logs.join("alpha@1.0.0.log"), DONE);
                fs::write(logs.join("alpha@1.0.0.log.rc"), "0\n").unwrap();
                let broken = w.work().join(target);
                if directory {
                    fs::remove_file(&broken).unwrap();
                    fs::create_dir(&broken).unwrap();
                    fs::write(broken.join("unowned"), "retained").unwrap();
                } else {
                    fs::write(&broken, [0xff]).unwrap();
                }
                let other = w.work().join(if target == RATCHET {
                    BACKSTOPS
                } else {
                    RATCHET
                });
                let other_before = fs::read(&other).unwrap();
                let mut plan = Plan::parse(&format!(
                    "{PLAN_MAGIC}\nlogdir {}\nratchet {}\nstore {}\nfile \ncrate alpha\talpha@1.0.0\tgate\n",
                    logs.display(), w.work().join(RATCHET).display(), w.store().display(),
                )).unwrap();
                let before = plan.facts.clone();
                let error = w
                    .conclude_without_grade_records(&mut plan, update)
                    .unwrap_err();
                assert!(error.0.contains(target), "{error}");
                assert_eq!(
                    plan.facts, before,
                    "no measurement is committed before policy inputs read"
                );
                assert!(!w.store().exists(), "no facts were filed");
                assert_eq!(fs::read(&other).unwrap(), other_before);
                if directory {
                    assert_eq!(
                        fs::read_to_string(broken.join("unowned")).unwrap(),
                        "retained"
                    );
                } else {
                    assert_eq!(fs::read(&broken).unwrap(), [0xff]);
                }
            }
        }
    }
}

#[test]
fn the_judge_rejects_present_unreadable_floor_inputs_before_writing() {
    for update in [false, true] {
        for target in [RATCHET, BACKSTOPS] {
            for directory in [false, true] {
                let w = World::new("judge-unreadable-floor-inputs");
                let a = w.prover("a");
                let head = w.commit(&[
                    ("src/lib.rs", "v1"),
                    (RATCHET, &floor(3)),
                    (BACKSTOPS, "alpha\t-\tcancelled\t0\n"),
                ]);
                let facts = w.file(&head, &a, 5, 9);
                let facts_before = fs::read(&facts).unwrap();
                let broken = w.work().join(target);
                if directory {
                    fs::remove_file(&broken).unwrap();
                    fs::create_dir(&broken).unwrap();
                    fs::write(broken.join("unowned"), "retained").unwrap();
                } else {
                    fs::write(&broken, [0xff]).unwrap();
                }
                let other = w.work().join(if target == RATCHET {
                    BACKSTOPS
                } else {
                    RATCHET
                });
                let other_before = fs::read(&other).unwrap();
                assert_eq!(
                    run_cmd(
                        &w.work(),
                        TrustCmd::Judge {
                            commit: "HEAD".into(),
                            prover: Some(a.bin.clone()),
                            update,
                            reason: None
                        }
                    ),
                    2
                );
                assert_eq!(fs::read(&facts).unwrap(), facts_before);
                assert_eq!(fs::read(&other).unwrap(), other_before);
                if directory {
                    assert_eq!(
                        fs::read_to_string(broken.join("unowned")).unwrap(),
                        "retained"
                    );
                } else {
                    assert_eq!(fs::read(&broken).unwrap(), [0xff]);
                }
            }
        }
    }
}

#[test]
fn the_judge_distinguishes_unreadable_stored_facts_from_absence() {
    let w = World::new("stored-facts-read-errors");
    let a = w.prover("a");
    let b = w.prover("b");
    let ratchet = floor(3);
    let head = w.commit(&[("src/lib.rs", "v1"), (RATCHET, &ratchet)]);
    w.file(&head, &b, 5, 9);
    let path = w.file(&head, &a, 5, 9);
    let shown = path.display().to_string();
    let check = |exit| {
        for update in [false, true] {
            let cmd = TrustCmd::Judge {
                commit: head.clone(),
                prover: Some(a.bin.clone()),
                update,
                reason: None,
            };
            assert_eq!(run_cmd(&w.work(), cmd), exit);
            assert_eq!(fs::read_to_string(w.work().join(RATCHET)).unwrap(), ratchet);
            assert!(!w.work().join(BACKSTOPS).exists());
        }
    };
    for directory in [false, true] {
        fs::remove_file(&path).unwrap();
        if directory {
            fs::create_dir(&path).unwrap();
        } else {
            fs::write(&path, [0xff]).unwrap();
        }
        check(2);
        let why = pending_for(&w.cli(), &a, "HEAD", false)
            .unwrap_err()
            .to_string();
        let cause = if directory {
            "not a regular file"
        } else {
            "utf-8"
        };
        assert!(why.contains(&shown) && why.contains(cause), "{why}");
    }
    fs::remove_dir(&path).unwrap();
    check(1);
    let why = pending_for(&w.cli(), &a, "HEAD", false)
        .unwrap()
        .unwrap_err();
    assert!(
        why.contains("no whole-tree Trust facts")
            && why.contains("holds facts for this tree from prover"),
        "{why}"
    );
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
    let head = w.commit(&[("src/lib.rs", "v1"), (MEMORY, "# no observations\n")]);
    w.file(&head, &a, 5, 9);
    let judge_cmd = |update| TrustCmd::Judge {
        commit: "HEAD".into(),
        prover: Some(a.bin.clone()),
        update,
        reason: None,
    };
    assert!(!w.work().join(RATCHET).exists());
    assert!(!w.work().join(BACKSTOPS).exists());
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
        Some(&format!("alpha\t4\t4\t{DRV}\n")),
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
    assert_eq!(bad.word, "COULD-NOT-RUN");
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
                Some(&format!("alpha\t4\t4\t{DRV}\n")),
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

/// DC-08: `trust log` takes one library, by name or `name@version`, and at most one
/// `--target-root DIR`. NEGATIVE CONTROL: a second library, a flag it does not take, a
/// `--target-root` given twice or with no directory, and no library at all are each refused,
/// naming what was wrong.
#[test]
fn the_log_verb_takes_one_library_and_a_target_root() {
    let parse = |args: &[&str]| parse_cmd(&mut std::iter::once("log").chain(args.iter().copied()));
    assert_eq!(
        parse(&["alpha"]),
        Ok(TrustCmd::Log {
            lib: "alpha".into(),
            root: None,
        })
    );
    assert_eq!(
        parse(&["--target-root", "/r", "alpha@1.0.0"]),
        Ok(TrustCmd::Log {
            lib: "alpha@1.0.0".into(),
            root: Some(PathBuf::from("/r")),
        })
    );
    for (args, said) in [
        (&["alpha", "beta"][..], "\"beta\""),
        (&["alpha", "--obligations"], "\"--obligations\""),
        (
            &["alpha", "--target-root", "/r", "--target-root", "/s"],
            "twice",
        ),
        (&["alpha", "--target-root"], "needs a directory"),
        (&["alpha", "--target-root", "--x"], "needs a directory"),
        (&[], "names one library"),
    ] {
        let why = parse(args).unwrap_err();
        assert!(why.contains(said), "{args:?}: {why}");
    }
}

#[test]
fn log_lookup_selects_regular_cargo_versions_and_reports_unusable_entries() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let log_path = |spec: &str| dir.path().join(format!("{spec}.log"));
    let lookup = |selector: &str| {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = print_log(dir.path(), selector, &mut out, &mut err);
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    };
    let row = "alpha+serde";
    let annotations = [
        "alpha+serde@local-notes",
        "alpha+serde@1.0.0-01",
        "alpha+serde@notes@4.0.0",
    ];
    for spec in annotations {
        testing::write_log(&log_path(spec), "private annotation\n");
        let (code, out, _) = lookup(spec);
        assert_eq!(code, 1, "{spec}");
        assert!(out.is_empty(), "{spec}: {out}");
    }
    let (code, out, _) = lookup(row);
    assert_eq!(code, 1);
    assert!(out.is_empty());

    let link = "alpha+serde@2.0.0";
    let directory = "alpha+serde@3.0.0";
    std::os::unix::fs::symlink(
        stored_log(&log_path(annotations[0])),
        stored_log(&log_path(link)),
    )
    .unwrap();
    for selector in [row, link] {
        let (code, out, err) = lookup(selector);
        assert_eq!(code, 2, "{selector}: {err}");
        assert!(out.is_empty(), "{selector}: {out}");
    }
    fs::create_dir(stored_log(&log_path(directory))).unwrap();
    let (code, out, err) = lookup(directory);
    assert_eq!(code, 2, "{err}");
    assert!(out.is_empty());

    let first = "alpha+serde@1.0.0-pre.log.2+build.007";
    testing::write_log(&log_path(first), "first check\n");
    for selector in [row, first] {
        assert_eq!(lookup(selector), (0, "first check\n".into(), String::new()));
    }
    let second = "alpha+serde@4.0.0";
    testing::write_log(&log_path(second), "second check\n");
    assert_eq!(lookup(second), (0, "second check\n".into(), String::new()));
    let (code, out, err) = lookup(row);
    assert_eq!(code, 2, "{err}");
    assert!(out.is_empty());
    assert!(err.contains(first) && err.contains(second), "{err}");
    for excluded in annotations.into_iter().chain([link, directory]) {
        assert!(!err.contains(excluded), "{err}");
    }
}

#[test]
fn only_live_unmeasured_results_show_matching_bounded_failed_sources() {
    use std::fmt::Write as _;
    let w = World::new("trust-failed-obligation-sources");
    let logs = w.root.join("logs");
    fs::create_dir_all(&logs).unwrap();
    let spec = "clock@1.0.0";
    let mut plan = Plan::parse(&format!(
        "{PLAN_MAGIC}\nlogdir {}\nratchet {}\ncrate clock\t{spec}\tgate\n",
        logs.display(),
        w.work().join(RATCHET).display()
    ))
    .unwrap();
    let log = logs.join(format!("{spec}.log"));
    let sidecar = logs.join(format!("{spec}.log.oblig.tsv"));
    fs::write(logs.join(format!("{spec}.log.rc")), "1\n").unwrap();
    let record = |failed: usize| {
        let total = failed + 1;
        let mut text = format!(
            "{}\npackage\tclock\nunit\tcargo-target(package=\"clock\",kind=[\"lib\"],target=\"clock\")\nfunctions\t{total}\ntotal\t{total}\nproved\t0\n",
            crate::trust_oblig::RECORD_MAGIC
        );
        for i in 0..failed {
            let location = if i == 1 {
                "@none\t0\t0\t0\t0"
            } else {
                "src/clock.rs\t48\t8\t49\t16"
            };
            let function = if i == 2 {
                "λ".repeat(DIAGNOSTIC_CHARS + 1)
            } else {
                format!("clock::shift_{i}")
            };
            writeln!(text, "o\t{function}\tassertion\t{location}\tfailed\t-\tPRIVATE ANCHOR\tPRIVATE DESCRIPTION").unwrap();
        }
        writeln!(text, "o\tclock::colocated_uncertain\thardened_panic_boundary\tsrc/clock.rs\t48\t8\t49\t16\tunknown\tunclassified\tboundary\tboundary\nend\t{total}").unwrap();
        text
    };
    let write_log = |failed, total, outcome| {
        testing::write_log(
            &log,
            format!(
                "Summary: 0 proved, {failed} failed ({total} total); {outcome}\nCoverage: {total}/{total} eligible function bodies verified (complete)\n"
            ),
        );
    };
    let verdict = |p: &mut Plan| {
        w.conclude_without_grade_records(p, Update::NONE)
            .unwrap()
            .verdict
    };

    fs::write(&sidecar, record(6)).unwrap();
    write_log(6, 7, "not-reproducible");
    let live = verdict(&mut plan);
    assert_eq!((live.exit, live.total.as_str()), (1, "0/0"));
    let report = &live.report;
    assert!(
        report.contains("FAILED OBLIGATIONS: clock\n  clock::shift_0 at src/clock.rs:48:8-49:16"),
        "{report}"
    );
    assert!(report.contains("clock::shift_1 (source location unavailable)"));
    assert!(report.contains(&format!(
        "{}… at src/clock.rs:48:8-49:16",
        "λ".repeat(DIAGNOSTIC_CHARS)
    )));
    assert!(report.contains("clock::shift_3") && report.contains("and 2 more"));
    for absent in ["clock::shift_4", "clock::colocated_uncertain", "PRIVATE"] {
        assert!(!report.contains(absent), "{report}");
    }
    let filed = plan.facts.to_record();
    assert!(!filed.contains("clock::shift"));
    let replay = judge(
        &Facts::parse(&filed).unwrap(),
        None,
        None,
        "R",
        "M",
        Update::NONE,
    );
    assert_eq!(
        (replay.exit, replay.total, replay.failed),
        (live.exit, live.total, live.failed)
    );
    assert!(!replay.report.contains("FAILED OBLIGATIONS:"));

    fs::write(&sidecar, record(1)).unwrap();
    write_log(1, 2, "not-reproducible");
    let one = verdict(&mut plan);
    assert!(one.report.contains("clock::shift_0") && !one.report.contains("and 2 more"));

    // Reuse the same live plan. A mismatched, empty or absent current record
    // withdraws the old locations even though the check still failed.
    fs::write(&sidecar, record(6)).unwrap();
    write_log(5, 7, "not-reproducible");
    assert!(!verdict(&mut plan).report.contains("FAILED OBLIGATIONS:"));
    write_log(0, 1, "not-reproducible");
    fs::write(&sidecar, record(0)).unwrap();
    assert!(!verdict(&mut plan).report.contains("FAILED OBLIGATIONS:"));
    write_log(6, 7, "not-reproducible");
    fs::remove_file(sidecar).unwrap();
    assert!(!verdict(&mut plan).report.contains("FAILED OBLIGATIONS:"));
}

/// A command copied from the live report selects that version in that run's directory.
#[test]
fn live_log_hints_open_the_measured_version_in_its_target_root() {
    let w = World::new("trust-log-hints");
    let root = w.root.join("run's evidence");
    let logs = root.join("logs");
    fs::create_dir_all(&logs).unwrap();
    let refused = format!("{TRUSTD_REFUSED}\n{TRUSTD_DIRTY}\n");
    let checks = [
        ("alpha@1.0.0", "still verifying\n", "1\n"),
        ("beta@2.0.0", refused.as_str(), "1\n"),
        (
            "unmeasured@3.0.0",
            "Result: COULD NOT RUN (outcome not-reproducible: not a measurement; exit 3)\n",
            "3\n",
        ),
        (
            "clock@1.0.0",
            concat!(
                "  [FAILED: unproved, no counterexample; failed closed] [assert] assertion: expect @ src/lib.rs:8:1-8:9\n",
                "  [FAILED] [assert] assertion: add @ src/lib.rs:14:1-14:9\n",
                "Summary: 0 proved, 2 failed (2 total); not-reproducible\n",
                "Coverage: 2/2 eligible function bodies verified (complete)\n",
            ),
            "1\n",
        ),
    ];
    for (spec, log, rc) in checks {
        testing::write_log(&logs.join(format!("{spec}.log")), log);
        fs::write(logs.join(format!("{spec}.log.rc")), rc).unwrap();
    }
    testing::write_log(&logs.join("alpha@0.9.0.log"), "an older check\n");
    testing::write_log(&logs.join("clock@0.9.0.log"), "a different clock version\n");
    fs::write(logs.join("alpha@1.0.0.log.budget"), "17\n").unwrap();
    let mut plan = Plan::parse(&format!(
        "{PLAN_MAGIC}\nlogdir {}\nratchet {}\ncrate alpha\talpha@1.0.0\tgate\ncrate beta\tbeta@2.0.0\tgate\ncrate clock\tclock@1.0.0\tgate\ncrate unmeasured\tunmeasured@3.0.0\tgate\n",
        logs.display(),
        w.work().join(RATCHET).display()
    ))
    .unwrap();
    let live = w
        .conclude_without_grade_records(&mut plan, Update::NONE)
        .unwrap();
    for (spec, expected, _) in checks {
        let hint = live
            .verdict
            .report
            .split('`')
            .find(|s| s.starts_with(&format!("targo --unverified ship trust log {spec} ")))
            .unwrap_or_else(|| panic!("{}", live.verdict.report));
        // Let the shell split the command exactly as it would when pasted into a terminal.
        let script = format!("targo() {{ printf '%s\\n' \"$@\"; }}\n{hint}");
        let output = aterm_uds::spawnfd::command("/bin/sh")
            .args(["-c", &script])
            .output()
            .unwrap();
        assert!(output.status.success(), "{hint}");
        let argv = String::from_utf8(output.stdout).unwrap();
        let args = argv.strip_prefix("--unverified\nship\ntrust\n").unwrap();
        let TrustCmd::Log {
            lib,
            root: Some(selected),
        } = parse_cmd(&mut args.lines()).unwrap()
        else {
            panic!("{argv:?}")
        };
        assert_eq!(selected, root);
        let mut out = Vec::new();
        assert_eq!(
            print_log(&selected.join("logs"), &lib, &mut out, &mut Vec::new()),
            0
        );
        assert_eq!(out, expected.as_bytes());
    }
    let replay = judge(&plan.facts, None, None, "R", "M", Update::NONE);
    assert_eq!(
        (replay.word, replay.exit),
        (live.verdict.word, live.verdict.exit)
    );
    assert!(replay.report.contains("log saved from the original run"));
    assert!(!replay.report.contains("ship trust log alpha"));
    let unmeasured = note(&replay, "CHECK LOG: unmeasured:");
    assert!(
        unmeasured.contains("log saved from the original run")
            && !unmeasured.contains("--target-root")
    );
    let clock = note(&replay, "CHECK LOG: clock:");
    assert!(clock.contains("log saved from the original run") && !clock.contains("--target-root"));
}

/// DC-08: a check's log is read from its stored form alone (`<name@version>.log.zst`,
/// decompressed): with none, the error names it; a stored log whose frame has not ended fails at
/// its end rather than read as whole; a symlink in its place is refused. NEGATIVE CONTROL: a
/// plain log beside it, as a lane before stored logs wrote one, is never read: a reader that
/// fell back to it would find a log, and its summary.
#[test]
fn a_checks_log_is_read_from_its_stored_form_alone() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let log = dir.path().join("alpha@1.0.0.log");
    fs::write(&log, "Summary: 1 proved, 0 failed (2 total)\n").unwrap();
    let missing = open_log(&log).err().unwrap();
    assert_eq!(missing.kind(), std::io::ErrorKind::NotFound);
    assert!(
        missing.to_string().contains("alpha@1.0.0.log.zst"),
        "{missing}"
    );
    testing::write_log(&log, "Summary: 3 proved, 0 failed (4 total)\n");
    let summary = Measured::of_log(open_log(&log).unwrap(), "0")
        .unwrap()
        .summary;
    assert!(summary.contains("3 proved"), "{summary}");
    let zst = fs::read(stored_log(&log)).unwrap();
    fs::write(stored_log(&log), &zst[..zst.len() - 4]).unwrap();
    let unended = Measured::of_log(open_log(&log).unwrap(), "0").unwrap_err();
    assert_eq!(
        unended.kind(),
        std::io::ErrorKind::UnexpectedEof,
        "{unended}"
    );
    fs::remove_file(stored_log(&log)).unwrap();
    std::os::unix::fs::symlink(&log, stored_log(&log)).unwrap();
    assert!(open_log(&log).is_err(), "a symlink is refused");
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
    // The missing checks can still contribute calculus proofs.
    let v = judge_with_credit(
        &lapsed(2, 1),
        Some(&floor3),
        None,
        ("R", "M", None),
        Update::NONE,
        None,
        &["CREDIT-REGRESSED".into()],
    );
    assert!(v.failed.is_empty());
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

    // Retry selection reads the committed baseline of the same stored measurement.
    let w = World::new("ambiguous-backstop-retry");
    let prover = w.prover("a");
    let known = kept_run("aterm-codec", &[], 131);
    let head = w.commit(&[("src/lib.rs", "v1"), (RATCHET, &floor), (BACKSTOPS, &known)]);
    testing::file(
        &w.cli(),
        &w.store(),
        &prover,
        &head,
        DRV,
        run(310, 131).crates,
    );
    let pending = || {
        pending_for(&w.cli(), &prover, "HEAD", false)
            .unwrap()
            .unwrap()
    };
    assert!(
        pending().is_empty(),
        "the known recurring cancellations leave a regression"
    );
    let ambiguous = format!(
        "{}aterm-codec\t-\tcancelled\t131\n",
        kept_run("aterm-codec", &[], 0)
    );
    w.commit(&[(BACKSTOPS, &ambiguous)]);
    assert_eq!(pending(), ["aterm-codec"]);
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
    testing::write_log(&beside(""), log);
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
    p.measure().unwrap();
    p.facts.driver = "/store/trust/15698/bin/targo".into();
    p.facts.driver_id = DRV.into();
    p.facts
}

/// Simulates the older counts-only log format for the clock, memory and spread rules.
fn legacy_check(
    name: &str,
    (spec, log, oblig): (&str, &str, &str),
    pace: Option<Pace>,
    record: bool,
) -> Facts {
    let mut legacy = String::new();
    for line in log.lines() {
        let line = if line.starts_with("Summary:") || line.starts_with("Stop causes:") {
            line.split(';').next().unwrap()
        } else {
            line
        };
        legacy.push_str(line);
        legacy.push('\n');
    }
    measured_check(name, (spec, &legacy, oblig), pace, record)
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
    // Trust explicitly disclaims the counts in both original logs. Even a matching
    // floor and recurring backstops cannot promote them into a measurement.
    for (name, check, proved, total) in [
        ("aterm-gpu", GPU, 812, 4685),
        ("aterm-render", RENDER, 1730, 20856),
    ] {
        let f = measured_check(name, check, Some(QUIET), true);
        let floor = format!("{name}\t{proved}\t{total}\t{DRV}\n");
        let backstops = kept_run(name, gated(&f).backstops.as_deref().unwrap(), 0);
        let v = judge(&f, Some(&floor), Some(&backstops), "R", "M", Update::RAISE);
        let restored = Facts::parse(&f.to_record()).unwrap();
        let replay = judge(
            &restored,
            Some(&floor),
            Some(&backstops),
            "R",
            "M",
            Update::RAISE,
        );
        let heading = format!("NOT MEASURED: {name}:");
        let diagnostic = note(&v, &heading);
        assert_eq!(note(&replay, &heading), diagnostic);
        let tally = check
            .1
            .lines()
            .find(|l| l.starts_with("Stop causes: "))
            .unwrap()
            .split_once(';')
            .unwrap()
            .0;
        assert!(diagnostic.contains(tally), "{diagnostic}");
        assert!(!diagnostic.contains("backstops fired:"));
        assert_eq!((v.word, v.total.as_str()), ("FAIL", "0/0"));
        assert_eq!(
            v.failed,
            [format!("{name} NOT-MEASURED(rc=1,not-reproducible)")]
        );
        assert_eq!((v.ratchet, v.backstops), (None, None));
    }
    let f = measured_check("aterm-gpu", GPU, Some(QUIET), false);
    assert_eq!(gated(&f).backstops, None, "no record, no names");
}

#[test]
fn only_the_final_non_measurement_explains_its_stops() {
    let note_for = |m| {
        let f = facts(vec![("alpha", Decision::Gate(m))]);
        let replay = Facts::parse(&f.to_record()).unwrap();
        let v = judge(&replay, None, None, "R", "M", Update::RAISE);
        assert_eq!((v.exit, v.total.as_str()), (1, "0/0"));
        assert!(v.unverified.is_empty() && v.ratchet.is_none());
        note(&v, "NOT MEASURED: alpha:").to_string()
    };
    let text = format!("{}{}", RENDER.1, GPU.1);
    let current = note_for(Measured::of_log(text.as_bytes(), "1").unwrap());
    assert!(current.contains("wall-backstop=89"), "{current}");
    assert!(!current.contains("rss-backstop=715"));
    let no_tally = "Summary: 5 proved (7 total); not-reproducible\nCoverage: 9/9 eligible function bodies verified (complete)\n";
    let m = Measured::of_log(format!("{text}{no_tally}").as_bytes(), "1").unwrap();
    assert!(!note_for(m.clone()).contains("Stop causes:"));
    let older = Measured {
        could_not_run: "COULD NOT RUN: earlier driver refusal".into(),
        ..m
    };
    assert!(!note_for(older).contains("earlier driver refusal"));
    let long = format!("{no_tally}Stop causes: {}\n", "長".repeat(2_000));
    let m = Measured::of_log(long.as_bytes(), "1").unwrap();
    assert_eq!(
        m.could_not_run
            .strip_prefix("Stop causes: ")
            .unwrap()
            .chars()
            .count(),
        DIAGNOSTIC_CHARS
    );
    let diagnostic = note_for(m);
    let cause = diagnostic.split_once(" (Stop causes: ").unwrap().1;
    assert!(cause.ends_with("…)") && cause.chars().count() <= DIAGNOSTIC_CHARS);
}

#[test]
fn unmeasured_failures_keep_the_final_compilers_counterexample_breakdown() {
    // Trust 27009's aterm-time report. Its four operator panic boundaries all failed closed.
    const SUMMARY: &str = "Summary: 6 proved, 4 failed, 36 runtime-checked, 12 assumed, 1 mandated, 0 contract-panic, 4 inconclusive (63 total); not-reproducible; profile c870797a6939; vector d3b1acba06fd";
    const SPLIT: &str = "Failed rows: 4 failed, 4 of them unproved with NO counterexample: nothing refuted them; they fail closed because Trust could not prove an unsafe operation or a Level-0 safety/panic-freedom obligation it may not let pass";
    const STOPS: &str = "Stop causes: not-dispatched=12 solved=10 unclassified=6 unsupported=35";
    const COVERAGE: &str = "Coverage: 56/56 eligible function bodies verified (complete)";
    let report = |split: &str, stops: &str| format!("{SUMMARY}\n{split}\n{stops}\n{COVERAGE}\n");
    let diagnostic = |log: &str| {
        let m = Measured::of_log(log.as_bytes(), "1").unwrap();
        let live = facts(vec![("aterm-time", Decision::Gate(m))]);
        let replay = Facts::parse(&live.to_record()).unwrap();
        let judge_note = |f: &Facts| {
            let verdict = judge(f, None, None, "R", "M", Update::RAISE);
            assert_eq!((verdict.exit, verdict.total.as_str()), (1, "0/0"));
            assert_eq!(
                verdict.failed,
                ["aterm-time NOT-MEASURED(rc=1,not-reproducible)"]
            );
            assert!(verdict.ratchet.is_none() && verdict.backstops.is_none());
            note(&verdict, "NOT MEASURED: aterm-time:").to_owned()
        };
        let said = judge_note(&live);
        assert_eq!(judge_note(&replay), said);
        said
    };

    let clock = report(SPLIT, STOPS);
    let said = diagnostic(&clock);
    assert!(
        said.contains("Failed rows: 4 failed, 4 unproved without a counterexample"),
        "{said}"
    );
    assert!(said.contains(STOPS), "{said}");
    assert!(!said.contains("because Trust"), "{said}");

    let mixed = report(&SPLIT.replace("4 of them", "2 of them"), "");
    let said = diagnostic(&format!("{clock}{mixed}"));
    assert!(
        said.contains("Failed rows: 4 failed, 2 unproved without a counterexample"),
        "{said}"
    );
    assert!(
        !said.contains("4 unproved") && !said.contains(STOPS),
        "{said}"
    );

    for unusable in [
        String::new(),
        SPLIT
            .replace("4 failed", "3 failed")
            .replace("4 of them", "2 of them"),
        SPLIT.replace("4 of them", "5 of them"),
        SPLIT.replace("4 of them", "0 of them"),
        SPLIT.replace("4 of them", "18446744073709551616 of them"),
        "Failed rows: 4 failed, 2 of them unproved".into(),
    ] {
        let said = diagnostic(&format!("{clock}{}", report(&unusable, STOPS)));
        assert!(!said.contains("Failed rows:"), "{unusable}: {said}");
        assert!(said.contains(STOPS), "{said}");
    }
}

#[test]
fn a_refuted_obligation_cannot_excuse_a_floor_regression_as_a_clock_stop() {
    let log =
        "Summary: 0 proved (1 total)\nCoverage: 1/1 eligible function bodies verified (complete)\n";
    for (status, verdict) in [("failed", "FAIL"), ("timeout", "COULD-NOT-RUN")] {
        // Old obligation sidecars could attach a marker from a counterexample to a refutation.
        let record = format!(
            "{}\npackage\talpha\nunit\tcargo-target(package=\"alpha\",target=\"alpha\",kind=[\"lib\"])\nfunctions\t1\ntotal\t1\nproved\t0\n\
             o\talpha::check\tassertion\tsrc/lib.rs\t1\t0\t1\t4\t{status}\twall-backstop\tcheck\tassertion\nend\t1\n",
            crate::trust_oblig::RECORD_MAGIC
        );
        let f = measured_check("alpha", ("alpha@0.0.0", log, &record), Some(STARVED), true);
        let floor = format!("alpha\t1\t1\t{DRV}\n");
        let v = judge(
            &f,
            Some(&floor),
            Some(&kept_run("alpha", &[], 0)),
            "R",
            "M",
            Update::NONE,
        );
        assert_eq!(v.word, verdict, "{}", v.report);
    }
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
    let all = gated(&legacy_check("aterm-gpu", GPU, None, true))
        .backstops
        .clone()
        .unwrap();
    let mut had = all.clone();
    let at = had.iter().position(|b| b.key == lost.key()).unwrap();
    had.remove(at);
    let had = kept_run("aterm-gpu", &had, 0);
    let judged = |pace: Pace, backstops: Option<&str>, update: Update<'_>| {
        let f = legacy_check("aterm-gpu", GPU, Some(pace), true);
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
    let f = legacy_check("aterm-gpu", GPU, Some(STARVED), true);
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
    let all = gated(&legacy_check("aterm-gpu", GPU, None, true))
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
        let f = legacy_check("aterm-gpu", GPU, Some(pace), true);
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
        Some(&format!("alpha\t4\t4\t{DRV}\n")),
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
        &legacy_check("aterm-gpu", GPU, Some(QUIET), true),
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
        &legacy_check("aterm-gpu", GPU, Some(QUIET), true),
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
        let f = alpha(stops, cancelled);
        let prior = kept_run("alpha", gated(&f).backstops.as_deref().unwrap(), cancelled);
        for backstops in [None, Some(prior.as_str())] {
            let v = judge(&f, Some(""), backstops, "R", "M", Update::RAISE);
            assert_eq!(row_of(&v, "alpha"), "NO-FLOOR", "{}", v.report);
            assert_eq!(v.exit, 1);
            assert!(note(&v, "RE-BASE WITHHELD: alpha").contains(&STARVED.said()));
            assert!(!v.report.contains("NO FLOOR: "), "{}", v.report);
            assert_eq!((v.ratchet, v.backstops), (None, None));
        }
        // The same stops from a real floor's run recur, so the higher count stands.
        let v = judge(&f, Some(&floor(2)), Some(&prior), "R", "M", Update::RAISE);
        assert_eq!(v.exit, 0, "{}", v.report);
        assert_eq!(v.ratchet, Some(floor(3)));
    }
    let ambiguous = "alpha\t-\tcancelled\t0\nalpha\t-\tcancelled\t1\n";
    let v = judge(
        &alpha(0, 1),
        Some(&floor(2)),
        Some(ambiguous),
        "R",
        "M",
        Update::RAISE,
    );
    assert_eq!(v.exit, 0, "the existing floor held: {}", v.report);
    assert!(note(&v, "RE-BASE WITHHELD: alpha").contains(&STARVED.said()));
    assert_eq!((v.ratchet, v.backstops), (None, None));

    let gpu = |pace: Pace| legacy_check("aterm-gpu", GPU, Some(pace), true);
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
    let f = legacy_check("aterm-render", RENDER, Some(STARVED), true);
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

    let bare = legacy_check("aterm-render", RENDER, Some(STARVED), false);
    assert_eq!(edge(1730 + 747 + 715 - 6), 3252, "3186 and the 65 of 3251");
    let v = judged(&bare, 3251, &wall);
    assert_eq!(row_of(&v, "aterm-render"), "NOT-VERIFIED(clock,<3251,1456)");
    let v = judged(&bare, 3252, &wall);
    assert_eq!(row_of(&v, "aterm-render"), "RATCHET-REGRESSION(<3252)");
    let quiet = legacy_check("aterm-render", RENDER, Some(QUIET), false);
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
    let head = "alpha\t-\tcancelled\t0";
    for replacement in [
        "alpha\t-\tcancelled\tsome",
        "alpha\t-\tcancelled\t0\nalpha\t-\tcancelled\t1",
        "alpha\t-\tcancelled\t1\nalpha\t-\tcancelled\t0",
        "alpha\t-\tcancelled\t0\nalpha\t-\tcancelled\t0",
        "alpha\t-\tcancelled\t0\nalpha\tkey\tunknown-stop\tf",
        "alpha\t-\tcancelled\t0\nalpha",
    ] {
        let damaged = text.replace(head, replacement);
        assert_eq!(floor_run(Some(&damaged), "alpha"), None, "{replacement:?}");
        assert_eq!(floor_run(Some(&damaged), "beta"), Some(FloorRun::default()));
    }
    // Look-alike obligations may share a key, and each occurrence counts.
    let stop = "alpha\t1\twall-backstop\tf1";
    let repeated = text.replace(stop, &format!("{stop}\n{stop}"));
    assert_eq!(
        floor_run(Some(&repeated), "alpha"),
        Some(run(&[b("1", "wall-backstop"), b("1", "wall-backstop")], 0))
    );

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
    let err = write_staged_text(&[
        (&floors, Some("new floors\n")),
        (&blocked, Some("x\n")),
        (&stops, Some("new stops\n")),
    ])
    .unwrap_err();
    assert!(err.to_string().contains("blocked.tsv"), "{err}");
    assert_eq!(fs::read_to_string(&floors).unwrap(), "old floors\n");
    assert_eq!(fs::read_to_string(&stops).unwrap(), "old stops\n");
    assert_eq!(names(), before, "a failed pass leaves no scratch");
    write_staged_text(&[(&floors, Some("new floors\n")), (&stops, None)]).unwrap();
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
    write_staged_text(&[(&floor, Some("ordinary update\n"))]).unwrap();
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
        let updated = write_staged_text(&[(&floor, Some("new memory\n"))]);
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

#[test]
fn overflowing_stored_credit_preserves_the_original_receipt() {
    let store = aterm_tempfile::tempdir().unwrap();
    let graded = |name, proved| {
        Crate::new(
            name,
            Decision::Gate(Measured {
                grade: GradeEvidence::Measured(crate::trust_credit::Tally {
                    proved,
                    graded: proved,
                    calculus: proved,
                }),
                ..Measured::default()
            }),
        )
    };
    let key = "overflow";
    let mut round = Facts {
        crates: vec![graded("alpha", 1), graded("beta", 1)],
        provenance: Some(Provenance {
            key: key.into(),
            head: "a".repeat(40),
            date: "2026-11-17T20:30:00Z".into(),
            ..Provenance::default()
        }),
        ..Facts::default()
    };
    let path = file_record(store.path(), &round).unwrap();
    let original = fs::read(&path).unwrap();
    round.crates = vec![graded("alpha", u64::MAX)];
    let error = complete_record(store.path(), key, &round).unwrap_err();
    assert!(
        error.contains("CREDIT-UNREADABLE") && error.contains("overflow"),
        "{error}"
    );
    assert_eq!(fs::read(&path).unwrap(), original);
    round.crates = vec![graded("alpha", u64::MAX - 1)];
    let (_, _, credit) = complete_record(store.path(), key, &round).unwrap();
    assert_eq!(credit.total.calculus, u64::MAX);
}

#[test]
fn parallel_remeasurements_keep_both_crates_in_one_receipt() {
    let store = aterm_tempfile::tempdir().unwrap();
    let key = "fixture";
    let base_row = |name| testing::gated(name, "Summary: 1 proved (4 total)");
    let base = Facts {
        provenance: Some(Provenance {
            key: key.into(),
            head: "a".repeat(40),
            date: "2026-11-17T20:30:00Z".into(),
            ..Provenance::default()
        }),
        crates: vec![base_row("alpha"), base_row("beta")],
        ..Facts::default()
    };
    let path = file_record(store.path(), &base).unwrap();
    let start = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        for name in ["alpha", "beta"] {
            let mut again = base.clone();
            again.crates = vec![testing::gated(name, "Summary: 4 proved (4 total)")];
            let start = &start;
            let store = store.path();
            scope.spawn(move || {
                start.wait();
                complete_record(store, key, &again).unwrap();
            });
        }
    });
    let saved = Facts::parse(&fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(
        saved.crates,
        [
            testing::gated("alpha", "Summary: 4 proved (4 total)"),
            testing::gated("beta", "Summary: 4 proved (4 total)"),
        ]
    );
    assert_eq!(saved.provenance.unwrap().remeasured.len(), 2);
}

#[test]
fn filed_facts_preserve_other_receipt_files() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let store = dir.path().join("receipts");
    let mut f = facts(vec![]);
    f.provenance = Some(Provenance {
        key: "fixture".into(),
        ..Provenance::default()
    });
    let path = file_record(&store, &f).unwrap();
    let occupied = store.join(format!(".{FACTS_PREFIX}fixture.{}", std::process::id()));
    fs::write(&occupied, "retained").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |file: &Path| fs::metadata(file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), mode(&occupied));
    }
    f.measured_at = "later".into();
    file_record(&store, &f).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), f.to_record());
    assert_eq!(fs::read_to_string(&occupied).unwrap(), "retained");

    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    let retained = path.join("other");
    fs::write(&retained, "keep").unwrap();
    let err = file_record(&store, &f).unwrap_err();
    assert!(err.contains(&path.display().to_string()), "{err}");
    assert_eq!(fs::read_to_string(retained).unwrap(), "keep");
    assert_eq!(fs::read_to_string(occupied).unwrap(), "retained");
}

#[test]
fn admission_distinguishes_absent_policy_files_from_unreadable_ones() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let obs = dir.path().join("observations.tsv");
    let mut plan = Plan {
        facts: facts(vec![("alpha", gate(""))]),
        ratchet: dir.path().join("floors.tsv"),
        memory: dir.path().join("memory.tsv"),
        host_obs: Some(obs.clone()),
        ..Plan::default()
    };
    plan.facts.only = "alpha".into();
    plan.facts.host.mem = 24 << 30;
    let admit = || plan.admit(&BTreeMap::new());
    let memory = "alpha\t4\t24\t3\t3\t>\tnow\n";
    fs::write(&plan.memory, memory).unwrap();
    assert!(admit().unwrap().1.is_empty(), "no floor has been measured");

    fs::write(&plan.ratchet, format!("alpha\t3\t5\t{DRV}\n")).unwrap();
    let (_, refusals) = admit().unwrap();
    assert_eq!(refusals.len(), 1);
    assert_eq!(refusals[0].0, "alpha");
    fs::write(&obs, format!("alpha\theld\t3\t2\t3\t3\t{DRV}\tnow\n")).unwrap();
    assert!(admit().unwrap().1.is_empty(), "this host held the floor");

    for path in [&obs, &plan.ratchet, &plan.memory] {
        let old = fs::read(path).unwrap();
        fs::write(path, [0xff]).unwrap();
        let cause = crate::durable::read_text(path).unwrap_err().to_string();
        let error = admit().unwrap_err();
        let shown = path.display().to_string();
        assert!(error.contains(&shown) && error.contains(&cause), "{error}");
        fs::write(path, old).unwrap();
    }
}

#[test]
fn host_observations_preserve_prior_rows_and_other_files() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("state/observations.tsv");
    let plan = Plan::default();
    let mut verdict = judge(&plan.facts, None, None, "R", "M", Update::NONE);
    verdict.host_obs = vec![(
        "alpha".into(),
        Some(["held", "-", "-", "3", "3"].map(String::from)),
    )];
    write_host_obs(&path, &plan, &verdict).unwrap();
    let occupied = path.with_extension(format!("tsv.{}", std::process::id()));
    fs::write(&occupied, "retained").unwrap();
    fs::write(&path, "beta\tother\nalpha\told\n").unwrap();
    write_host_obs(&path, &plan, &verdict).unwrap();
    let rows = fs::read_to_string(&path).unwrap();
    assert!(rows.contains("beta\tother\n") && rows.contains("alpha\theld\t"));
    assert!(!rows.contains("alpha\told"));
    assert_eq!(fs::read_to_string(occupied).unwrap(), "retained");
}

#[test]
fn unreadable_host_observations_survive_an_update() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("observations.tsv");
    let plan = Plan::default();
    let mut verdict = judge(&plan.facts, None, None, "R", "M", Update::NONE);
    verdict.host_obs = vec![(
        "alpha".into(),
        Some(["held", "-", "-", "3", "3"].map(String::from)),
    )];
    let prior = b"beta\t\xff\n";
    fs::write(&path, prior).unwrap();
    let err = write_host_obs(&path, &plan, &verdict).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(
        err.to_string().contains(&path.display().to_string()),
        "{err}"
    );
    assert_eq!(fs::read(path).unwrap(), prior);
}

/// A failed first stage never acquired a later scratch name; cleanup cannot unlink it.
#[test]
fn floor_staging_failure_preserves_an_unvisited_occupied_name() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let ordinary = dir.path().join("ordinary.tsv");
    write_staged_text(&[(&ordinary, Some("ordinary update\n"))]).unwrap();
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
        write_staged_text(&[
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
    base.host = host(32);
    again.host = base.host;
    base.host.reach = base.host.mem / 2;
    let base = Facts::parse(&base.to_record()).unwrap();
    assert_eq!(base.host.reach, 0);
    again.host.reach = again.host.mem / 3;
    assert_ne!(base.host.reach, again.host.reach);
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
    let mut other_source = again.clone();
    other_source.host.memtotal = !again.host.memtotal;
    assert!(
        remeasured(&base, &other_source)
            .unwrap_err()
            .contains("MemTotal")
    );
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
    let v = judge_with_credit(
        &f,
        Some(&format!("aterm-cli\t75\t443\t{DRV}\n")),
        None,
        ("R", "M", None),
        Update::NONE,
        None,
        &["CREDIT-REGRESSED".into()],
    );
    assert!(v.failed.is_empty());
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
    let m = Measured::of_log(REFUSED_LOG.as_bytes(), "2").unwrap();
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
        let m = Measured::of_log(log.as_bytes(), "2").unwrap();
        assert_eq!(m.path_refused, [want], "{text}");
    }
    let m = Measured::of_log(REFUSED_TRUSTD.as_bytes(), "2").unwrap();
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
        let m = Measured::of_log(text.as_bytes(), "1").unwrap();
        assert!(m.path_refused.is_empty(), "{text}: {:?}", m.path_refused);
    }
}

#[test]
fn a_refused_path_is_said_once_with_every_library_it_failed() {
    let me = aterm_uds::peer::our_uid();
    let mine = REFUSED_LOG.replace("uid=1000", &format!("uid={me}"));
    let gated = |text: &str| Decision::Gate(Measured::of_log(text.as_bytes(), "2").unwrap());
    let ratchet = format!("eps\t5\t21\t{DRV}\n");
    let v = judge(
        &facts(vec![
            ("alpha", gated(&mine)),
            ("beta", gated(&mine)),
            ("gamma", gated(REFUSED_TMP)),
            ("delta", gated(REFUSED_TRUSTD)),
            ("eps", Decision::Gate(log(DONE))),
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
        &facts(vec![("eps", Decision::Gate(log(DONE)))]),
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
        testing::write_log(&logs.join("alpha@1.0.0.log"), log);
        fs::write(logs.join("alpha@1.0.0.log.rc"), rc).unwrap();
        let mut p = Plan::parse(&plan).unwrap();
        w.conclude_without_grade_records(&mut p, Update::NONE)
            .unwrap()
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
