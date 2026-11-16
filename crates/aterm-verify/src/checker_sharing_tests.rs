// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::plan::StageId;

#[test]
fn checker_sharing_requires_an_actual_checker_enabled_test() {
    for release in [false, true] {
        for enabled in [false, true] {
            for selected in [
                vec![Check::Test, Check::Checkers],
                vec![Check::Checkers, Check::Test],
                vec![Check::Checkers],
            ] {
                let ctx = Ctx::new(
                    PathBuf::from("/repo"),
                    Scope::crate_only("aterm-types"),
                    EnvSnapshot::default(),
                    PathBuf::from("/tmp"),
                )
                .with_checks(&selected)
                .with_release(release)
                .with_checkers_in_tests(enabled);
                let planned = plan::plan(&ctx);
                let shared = enabled && selected.contains(&Check::Test);
                assert_eq!(
                    planned.iter().any(|s| s.id == StageId::SpecCheckers),
                    !shared,
                    "{selected:?}, release={release}, enabled={enabled}"
                );
                assert_eq!(
                    checks::owners(StageId::Test, &selected, enabled).contains(&Check::Checkers),
                    shared
                );
                assert_eq!(
                    checks::owners(StageId::SpecCheckers, &selected, enabled)
                        .contains(&Check::Checkers),
                    !shared
                );
            }
        }
    }
}

#[test]
fn one_recorded_execution_decides_both_checks_even_when_it_fails_or_cannot_run() {
    use crate::ladder::{Outcome, Severity};
    use std::sync::Mutex;

    static CALLED: Mutex<Vec<StageId>> = Mutex::new(Vec::new());

    fn observed(spec: &plan::StageSpec, outcome: Outcome, at: StageId) -> Report {
        CALLED.lock().unwrap().push(spec.id);
        let mut report = Report::new(spec.title.clone());
        match (spec.id == at, outcome) {
            (true, Outcome::Fail(Severity::GateFailed)) => report.fail("planted shared failure"),
            (true, Outcome::Fail(Severity::CouldNotRun)) => {
                report.cannot_run("planted unavailable runtime")
            }
            (true, Outcome::Skip) => report.skip("planted skipped runtime"),
            _ => report.pass("observed execution"),
        }
        report
    }
    fn pass(_: &Ctx, spec: &plan::StageSpec) -> Report {
        observed(spec, Outcome::Ok, StageId::Test)
    }
    fn fail(_: &Ctx, spec: &plan::StageSpec) -> Report {
        observed(spec, Outcome::Fail(Severity::GateFailed), StageId::Test)
    }
    fn cannot_run(_: &Ctx, spec: &plan::StageSpec) -> Report {
        observed(spec, Outcome::Fail(Severity::CouldNotRun), StageId::Test)
    }
    fn skip(_: &Ctx, spec: &plan::StageSpec) -> Report {
        observed(spec, Outcome::Skip, StageId::Test)
    }
    fn compile_fails(_: &Ctx, spec: &plan::StageSpec) -> Report {
        observed(
            spec,
            Outcome::Fail(Severity::GateFailed),
            StageId::TestCompile,
        )
    }
    type Case = (fn(&Ctx, &plan::StageSpec) -> Report, Outcome);
    let cases: [Case; 5] = [
        (pass, Outcome::Ok),
        (fail, Outcome::Fail(Severity::GateFailed)),
        (cannot_run, Outcome::Fail(Severity::CouldNotRun)),
        (skip, Outcome::Skip),
        (compile_fails, Outcome::Fail(Severity::GateFailed)),
    ];
    for (stage, expected) in cases {
        CALLED.lock().unwrap().clear();
        let dir = aterm_tempfile::tempdir().unwrap();
        let ctx = Ctx::new(
            dir.path().to_path_buf(),
            Scope::crate_only("aterm-types"),
            EnvSnapshot::default(),
            dir.path().join("scratch"),
        )
        .with_checks(&[Check::Test, Check::Checkers])
        .with_checkers_in_tests(true);
        let mut output = Vec::new();
        run_with(
            &ctx,
            &mut output,
            &mut Vec::new(),
            &Show::default(),
            Seams {
                stage,
                prepare_cells: None,
                prepare_build_dir: |_| Ok(()),
                ..LIVE
            },
        )
        .unwrap();
        let called = CALLED.lock().unwrap();
        assert_eq!(called.iter().filter(|id| **id == StageId::Test).count(), 1);
        assert!(!called.contains(&StageId::SpecCheckers));
        let output = String::from_utf8(output).unwrap();
        let checks: Vec<aterm_json::Value> = output
            .lines()
            .filter_map(|line| line.strip_prefix("verify-event: "))
            .map(|json| aterm_json::from_str(json).unwrap())
            .filter(|event: &aterm_json::Value| event["event"].as_str() == Some("check-finished"))
            .collect();
        assert_eq!(checks.len(), 2, "{output}");
        for check in [Check::Test, Check::Checkers] {
            let record = checks
                .iter()
                .find(|record| record["check"].as_str() == Some(check.name()))
                .unwrap();
            assert_eq!(
                record["outcome"].as_str(),
                Some(report::word(expected)),
                "{record}"
            );
            assert!(record["rows_total"].as_u64().unwrap() >= 2, "{record}");
        }
    }
}

#[test]
fn a_shared_checker_key_is_bound_to_its_actual_runtime_stage() {
    let mut ctx = Ctx::new(
        PathBuf::from("/repo"),
        Scope::crate_only("aterm-types"),
        EnvSnapshot::default(),
        PathBuf::from("/tmp"),
    )
    .with_checks(&[Check::Test, Check::Checkers])
    .with_checkers_in_tests(true);
    let shared = checks::part(Check::Checkers, &ctx, "base", "changed");
    ctx.checks = vec![Check::Checkers];
    assert_ne!(
        shared,
        checks::part(Check::Checkers, &ctx, "base", "changed")
    );
    ctx.checks = vec![Check::Test, Check::Checkers];
    ctx.checkers_in_tests = false;
    assert_ne!(
        shared,
        checks::part(Check::Checkers, &ctx, "base", "changed")
    );
}

#[test]
fn compile_only_keeps_its_shared_compile_separate_from_runtime_evidence() {
    let mut ctx = Ctx::new(
        PathBuf::from("/repo"),
        Scope::crate_only("aterm-types"),
        EnvSnapshot::default(),
        PathBuf::from("/tmp"),
    )
    .with_checks(&[Check::Test, Check::Checkers])
    .with_checkers_in_tests(true);
    let runtime_key = checks::part(Check::Checkers, &ctx, "base", "changed");
    ctx.compile_only = true;
    let planned = plan::plan(&ctx);
    assert_eq!(planned.len(), 1);
    assert_eq!(planned[0].id, StageId::TestCompile);
    assert_eq!(
        checks::owners(StageId::TestCompile, &ctx.checks, ctx.checkers_in_tests),
        [Check::Test, Check::Checkers]
    );
    assert_ne!(
        runtime_key,
        checks::part(Check::Checkers, &ctx, "base", "changed")
    );
}
