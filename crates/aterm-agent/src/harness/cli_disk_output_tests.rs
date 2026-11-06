// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Disk output faults after the report and any authorized private action are complete.

use super::*;
use crate::harness::cli::disk_output::DiskOutput;

fn fixture(
    apply: bool,
) -> (
    Tmp,
    Env,
    disk::Report,
    DiskWatchStatus,
    Option<disk::Applied>,
) {
    let tmp = tmp_dir("disk-output");
    let env = env_at(&tmp);
    let cache = tmp.path().join("cache");
    std::fs::create_dir(&cache).expect("owned fixture directory");
    std::fs::write(cache.join("blob"), vec![1u8; 4096]).expect("owned fixture bytes");
    let mut survey = disk::Survey::new(env.now);
    survey.root = tmp.path().to_path_buf();
    let mut report = disk::report(
        &survey,
        disk::Config {
            apply,
            ..disk::Config::default()
        },
    );
    report.rows = vec![disk::Row {
        class: disk::Class::CargoTargets,
        path: cache.clone(),
        bytes: 4096,
        bytes_partial: false,
        witness: disk::Witness::StaleBuildDir {
            marker: "Cargo.toml".into(),
            profiles: 1,
            idle_days: 99,
            threshold_days: 30,
            leaves: Vec::new(),
        },
        removable: true,
        blocked: None,
    }];
    report.notes = vec!["fixture note".into()];
    report.hogs = vec![disk::Hog {
        path: tmp.path().join("kept"),
        bytes: 8192,
        bytes_partial: false,
        why: "fixture holder".into(),
        serves: None,
        built_s_ago: None,
    }];
    std::fs::create_dir_all(&env.state).expect("journal directory");
    let mut err = Vec::new();
    let path = env.disk_ledger();
    let mut journal = Journal::open(Some(&path), Some(&env.sid), &mut err);
    assert!(journal.append_raw(&disk::report_row(env.now, &env.sid, &report), &mut err));
    let applied = apply.then(|| {
        disk::apply(
            &report,
            None,
            Some(disk::Class::CargoTargets),
            &mut |row| {
                assert_eq!(row.path, cache, "only this test's directory may be removed");
                std::fs::remove_file(cache.join("blob"))?;
                std::fs::remove_dir(&cache)?;
                Ok(disk::Removed {
                    bytes: row.bytes,
                    units: vec![cache.clone()],
                    ..disk::Removed::default()
                })
            },
            &mut || panic!("a non-pressure fixture must not query the volume"),
            &mut |step| journal.append_raw(&disk::step_row(env.now, &env.sid, step), &mut err),
        )
    });
    if let Some(done) = &applied {
        assert_eq!(done.removed.len(), 1, "{done:?}");
        assert!(done.denials.is_empty(), "{done:?}");
        assert_eq!(done.freed_bytes, 4096);
    }
    assert!(err.is_empty(), "{}", String::from_utf8_lossy(&err));
    assert_eq!(cache.exists(), !apply);
    let watch = DiskWatchStatus {
        line: "watch: fixture".into(),
        json: aterm_json::json!({"kind":"disk-watch","state":"fixture"}),
    };
    (tmp, env, report, watch, applied)
}

fn output_failure(flush: bool) {
    let mut failures = Vec::new();
    for apply in [false, true] {
        for json in [false, true] {
            let (tmp, env, report, watch, applied) = fixture(apply);
            let output = DiskOutput {
                report: &report,
                watch: &watch,
                applied: applied.as_ref(),
                json,
                config_path: env.config.as_deref(),
                off_by_fault: false,
            };
            let ledger = std::fs::read(env.disk_ledger()).expect("completed journal");
            let before = applied.clone();
            let (mut complete, mut err) = (Vec::new(), Vec::new());
            assert_eq!(output.print(&mut complete, &mut err), ExitCode::SUCCESS);
            assert!(err.is_empty());
            let limits = if flush {
                vec![None]
            } else {
                let first = complete
                    .iter()
                    .position(|b| *b == b'\n')
                    .expect("result row")
                    + 1;
                vec![Some(0), Some(1), Some(first + 1), Some(complete.len() - 1)]
            };
            for remaining in limits {
                let mut sink = ResultSink {
                    bytes: Vec::new(),
                    remaining,
                    fail_flush: flush,
                    flushed: false,
                };
                err.clear();
                let code = output.print(&mut sink, &mut err);
                let diagnostic = String::from_utf8_lossy(&err);
                if code != ExitCode::from(1)
                    || !diagnostic.contains("harness disk:")
                    || !diagnostic.contains(OUTPUT_FAILURE)
                    || sink.flushed != flush
                {
                    failures.push(format!(
                        "apply={apply} json={json} remaining={remaining:?} flush={flush}: \
                         code={code:?}, flushed={}, err={diagnostic:?}",
                        sink.flushed
                    ));
                }
                assert_eq!(sink.bytes, complete[..remaining.unwrap_or(complete.len())]);
                assert_eq!(applied, before);
                assert_eq!(std::fs::read(env.disk_ledger()).unwrap(), ledger);
                assert_eq!(tmp.path().join("cache").exists(), !apply);
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn disk_report_output_write_failures_preserve_completed_actions_and_journal() {
    output_failure(false);
}

#[test]
fn disk_report_output_flush_failures_preserve_completed_actions_and_journal() {
    output_failure(true);
}

#[test]
fn disk_report_output_success_flushes_complete_results() {
    for apply in [false, true] {
        for json in [false, true] {
            let (tmp, env, report, watch, applied) = fixture(apply);
            let output = DiskOutput {
                report: &report,
                watch: &watch,
                applied: applied.as_ref(),
                json,
                config_path: env.config.as_deref(),
                off_by_fault: false,
            };
            let ledger = std::fs::read(env.disk_ledger()).unwrap();
            let (mut complete, mut err) = (Vec::new(), Vec::new());
            assert_eq!(output.print(&mut complete, &mut err), ExitCode::SUCCESS);
            assert!(err.is_empty());
            let mut sink = ResultSink {
                bytes: Vec::new(),
                remaining: None,
                fail_flush: false,
                flushed: false,
            };
            assert_eq!(output.print(&mut sink, &mut err), ExitCode::SUCCESS);
            assert!(err.is_empty());
            assert_eq!(sink.bytes, complete);
            assert!(sink.flushed);
            if json {
                let records: Vec<Value> = String::from_utf8(complete)
                    .unwrap()
                    .lines()
                    .map(|l| aterm_json::from_str(l).expect("JSON result row"))
                    .collect();
                assert_eq!(records.len(), if apply { 3 } else { 2 });
                assert_eq!(records[0]["kind"], Value::from("disk"));
                assert_eq!(records[1]["kind"], Value::from("disk-watch"));
                if apply {
                    assert_eq!(records[2]["freed_bytes"], Value::from(4096u64));
                    assert_eq!(records[2]["removed"], Value::from(1u64));
                }
            }
            assert_eq!(std::fs::read(env.disk_ledger()).unwrap(), ledger);
            assert_eq!(tmp.path().join("cache").exists(), !apply);
        }
    }
}

#[test]
fn disk_report_output_delivered_refusal_preserves_existing_status() {
    for json in [false, true] {
        let (tmp, env, report, watch, _) = fixture(false);
        let mut err = Vec::new();
        let path = env.disk_ledger();
        let mut journal = Journal::open(Some(&path), Some(&env.sid), &mut err);
        let done = disk::apply(
            &report,
            None,
            Some(disk::Class::CargoTargets),
            &mut |_| panic!("a refused apply must not remove anything"),
            &mut || panic!("a refused apply must not measure the volume"),
            &mut |step| journal.append_raw(&disk::step_row(env.now, &env.sid, step), &mut err),
        );
        assert_eq!(
            done.denials,
            [disk::Refusal::ApplyOff(disk::Class::CargoTargets)]
        );
        assert!(done.removed.is_empty());
        let ledger = std::fs::read(&path).unwrap();
        let mut sink = ResultSink {
            bytes: Vec::new(),
            remaining: None,
            fail_flush: false,
            flushed: false,
        };
        assert_eq!(
            DiskOutput {
                report: &report,
                watch: &watch,
                applied: Some(&done),
                json,
                config_path: None,
                off_by_fault: true,
            }
            .print(&mut sink, &mut err),
            ExitCode::SUCCESS,
            "delivery status must not change the existing apply-refusal policy"
        );
        assert!(err.is_empty());
        assert!(sink.flushed);
        assert_eq!(std::fs::read(&path).unwrap(), ledger);
        assert!(tmp.path().join("cache/blob").is_file());
    }
}
