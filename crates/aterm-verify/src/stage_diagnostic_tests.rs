// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// The actual scheduler and EventLog publish a finished stage's diagnostic while a
/// sibling still owns its turn. Every assertion follows explicit release and join.
#[test]
fn a_stage_diagnostic_is_durable_before_its_held_sibling_finishes() {
    fn stage(ctx: &Ctx, spec: &plan::StageSpec) -> Report {
        let mut report = Report::new(spec.title.clone());
        let until = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        if spec.id == plan::StageId::Formatting {
            std::fs::write(ctx.scratch.join("sibling-started"), "").unwrap();
            let release = ctx.scratch.join("release-sibling");
            while !release.exists() && std::time::Instant::now() < until {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(
                release.exists(),
                "the parent must explicitly release the sibling"
            );
            report.pass("held sibling completed");
        } else {
            while !ctx.scratch.join("sibling-started").exists() && std::time::Instant::now() < until
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            report.fail("synthetic compiler cell — TYPE-ERROR");
            report.raw("warning: synthetic owned diagnostic\n  --> crates/fixture/src/lib.rs:7:9");
        }
        report
    }
    let dir = aterm_tempfile::tempdir().unwrap();
    let events = dir.path().join("stage.events");
    let ctx = Ctx::new(
        dir.path().into(),
        Scope::crate_only("aterm-verify"),
        EnvSnapshot::default(),
        dir.path().into(),
    )
    .with_checks(&[Check::Fmt, Check::Census])
    .with_events(Some(event_log::EventLog::open(&events).unwrap()));
    let (early, held, result, stdout) = std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            let mut stdout = Vec::new();
            let result = run_with(
                &ctx,
                &mut stdout,
                &mut std::io::sink(),
                &Show::default(),
                Seams {
                    stage,
                    prepare_cells: None,
                    ..LIVE
                },
            );
            (result, stdout)
        });
        let until = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        let mut early = None;
        while std::time::Instant::now() < until {
            let text = std::fs::read_to_string(&events).unwrap_or_default();
            early = text
                .split_inclusive('\n')
                .filter_map(|line| {
                    let json = line.strip_suffix('\n')?.strip_prefix("verify-event: ")?;
                    let event: aterm_json::Value = aterm_json::from_str(json).ok()?;
                    (event["event"].as_str() == Some("stage-finished")
                        && event["finding_diagnostics"]
                            .to_string()
                            .contains("synthetic owned diagnostic"))
                    .then_some(event)
                })
                .next();
            if early.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let held = !worker.is_finished() && dir.path().join("sibling-started").exists();
        // Release even when the observation is absent; a broken emitter cannot strand a sibling.
        std::fs::write(dir.path().join("release-sibling"), "").unwrap();
        let (result, stdout) = worker.join().unwrap();
        (early, held, result, stdout)
    });
    assert!(
        held,
        "diagnostic capture precedes completion of the sibling"
    );
    let event = early.expect("the durable stage diagnostic before release");
    assert_eq!(event["check"].as_str(), Some("census"));
    assert!(event["stage_index"].as_u64().is_some());
    assert!(event["stage"].as_str().unwrap().contains("source guards"));
    assert_eq!(
        event_log::diagnostic_invocation_id(&event).as_deref(),
        ctx.events.as_ref().and_then(event_log::EventLog::id)
    );
    assert_eq!(event["decisions"][0]["outcome"].as_str(), Some("FAIL"));
    assert_eq!(
        event["findings"][0].as_str(),
        Some("synthetic compiler cell — TYPE-ERROR")
    );
    assert_eq!(result.unwrap(), exit::FAILED);
    let text = String::from_utf8(stdout).unwrap();
    assert!(text.contains("held sibling completed"));
    assert!(text.contains("synthetic owned diagnostic"));
    assert!(text.contains("verify: FAIL"));
}
