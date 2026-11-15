// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Exact-fixture functional sizing. This command grants no timing authority.

use super::*;
use crate::timing_comparison::{FOOTPRINT_SEMANTICS, FootprintObservation};
use aterm_json::{Value, json};
use std::collections::BTreeMap;

#[path = "title_drift_sizing_capture.rs"]
mod capture;

const HELP: &str = "usage: lab title-drift-memory [title-drift build/work/case options] --out NEW_ABSOLUTE_DIR --calibration-memory-mib N --bound-growth-mib N --bound-reason TEXT\n\nRuns the exact frozen title fixture under ordinary functional memory admission.\nBoth budgets must be explicit and positive. The functional budget must cover the\nwhole owned process envelope plus the ordinary separate spare. Growth is additional\nper-component headroom beyond the observed peak plus a fifth; explain its concrete\njustification (including sampling gaps and later controller evidence parsing).\nDefault cases: 1,30,200,split; default repetitions: 3 ABBA.\nProduces identity-bound raw sizing sweeps and a frozen memory-record.json only after\nevery requested fixture and exact cleanup succeeds. Never qualifies timing or emits\naccepted.csv. This opens native windows; respect the ordinary window/resource lane.\n";

fn positive_mib(args: &mut Vec<String>, name: &str) -> Result<u64, Fail> {
    let text = take_value(args, name)?
        .ok_or_else(|| Fail::usage(format!("{name} is required; no default sizing authority")))?;
    text.parse::<u64>()
        .ok()
        .filter(|n| *n > 0)
        .and_then(|n| n.checked_mul(1024 * 1024))
        .ok_or_else(|| Fail::usage(format!("{name} must be positive MiB without overflow")))
}

pub(crate) fn cmd(args: &[String]) -> Res {
    if args.iter().any(|s| s == "--help" || s == "-h") {
        print!("{HELP}");
        return Ok(0);
    }
    let mut args = args.to_vec();
    let budget = positive_mib(&mut args, "--calibration-memory-mib")?;
    let growth = positive_mib(&mut args, "--bound-growth-mib")?;
    let reason = take_value(&mut args, "--bound-reason")?
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            Fail::usage(
                "--bound-reason must justify growth, sampling gaps and controller parsing overhead",
            )
        })?;
    let o = parse(&args, Purpose::MemorySizing)?;
    run(&o, budget, growth, &reason).map_err(|why| Fail::exit(3, why))?;
    println!(
        "{}",
        json!({"footprint_semantics":FOOTPRINT_SEMANTICS,"kind":"functional_memory_sizing","timing_qualified":false,"memory_record":o.out.join("memory-record.json").display().to_string()})
    );
    Ok(0)
}

fn run(o: &Options, budget: u64, growth: u64, reason: &str) -> Result<(), String> {
    let experiments = o
        .cases
        .iter()
        .map(|case| experiment_binding(o, *case))
        .collect::<Result<Vec<_>, _>>()?;
    std::fs::create_dir(&o.out).map_err(|e| format!("new sizing output directory: {e}"))?;
    stop::install();
    let mut comparisons = Vec::new();
    for (case, experiment) in o.cases.iter().zip(&experiments) {
        let mut sources = Vec::new();
        let mut observations: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        for ordinal in 0..experiment.expected_arms() {
            let arm = crate::aos_cadence::ORDER[ordinal % 4];
            let build = &o.builds[usize::from(arm == 'B')];
            let stem = o.out.join(format!("{}-{ordinal:03}-{arm}", case.label()));
            let mut raw = Raw::default();
            raw.note(
                "purpose",
                "functional_memory_sizing; timing_qualified=false",
            );
            raw.note(
                "workload",
                format!(
                    "case={} ordinal={ordinal} arm={arm} work={:?}",
                    case.label(),
                    o.work
                ),
            );
            let result = sizing_take(o, *case, build, experiment, budget, &stem, &mut raw);
            raw.note(
                "result",
                format!(
                    "{:?}",
                    result
                        .as_ref()
                        .map(|_| "functional fixture and cleanup complete")
                ),
            );
            if let Some(cast) = &raw.1 {
                crate::util::write_fresh(&stem.with_extension("cast"), cast)
                    .map_err(|e| e.to_string())?;
            }
            crate::util::write_fresh(&stem.with_extension("txt"), &raw.0)
                .map_err(|e| e.to_string())?;
            let (report, generation) = result?;
            experiment.validate()?;
            let source = json!({"footprint_semantics":FOOTPRINT_SEMANTICS,"kind":"functional_memory_sizing","timing_qualified":false,
                "experiment_sha256":experiment.key,"case":case.label(),"ordinal":ordinal,"arm":arm.to_string(),
                "binary_sha256":build.digest,"source_sha":build.source,"build_record_sha256":build.record_digest,
                "functional_budget_bytes":budget,"ordinary_spare_bytes":crate::admission::FUNCTIONAL_MARGIN,
                "fixture_complete":true,"cleanup_complete":true,"generation":generation,
                "raw_path":report.raw_path.display().to_string(),"raw_sha256":report.raw_digest,
                "fixture_raw_path":stem.with_extension("txt").display().to_string(),
                "fixture_raw_sha256":aterm_update_core::sha256_file(&stem.with_extension("txt"))?,
                "cast_path":stem.with_extension("cast").display().to_string(),
                "cast_sha256":aterm_update_core::sha256_file(&stem.with_extension("cast"))?,
                "witnesses":report.witnesses});
            let path = stem.with_extension("json");
            crate::util::write_fresh(&path, source.to_string()).map_err(|e| e.to_string())?;
            sources.push(json!({"path":path.display().to_string(),"sha256":aterm_update_core::sha256_file(&path)?}));
            for (name, values) in report.witnesses {
                observations.entry(name).or_default().extend(values);
            }
        }
        let _preparation =
            crate::admission::functional("title-drift memory evidence preparation", budget)
                .map_err(|f| {
                    f.msg
                        .unwrap_or_else(|| "functional preparation admission refused".into())
                })?;
        let mut components = Vec::new();
        for name in capture::COMPONENTS {
            let values = observations
                .get_mut(name)
                .ok_or("complete sizing lacks component")?;
            values.sort_by_key(|v| v.get("monotonic_ns").and_then(Value::as_u64));
            let evidence = json!({"footprint_semantics":FOOTPRINT_SEMANTICS,"experiment_sha256":experiment.key,"component":name,"observations":values,"captures":sources});
            let text = evidence.to_string();
            let peak = crate::timing_comparison::measured_peak(&text, experiment, name)?;
            let bound = bound(peak, growth)?;
            let path = o.out.join(format!("{}-{name}-evidence.json", case.label()));
            crate::util::write_fresh(&path, &text).map_err(|e| e.to_string())?;
            components.push(json!({"name":name,"bound_bytes":bound,
                "bound_derivation":format!("Observed current-reading sum floored by one member's kernel lifetime peak: {peak}; plus ceil(peak/5), plus explicit {growth} bytes growth per component. Counters are non-atomic and this is an observed lower bound, not a guaranteed unseen all-time aggregate peak. setup_cleanup_overlap deliberately duplicates the whole owned envelope. Justification: {reason}"),
                "evidence_path":path.display().to_string(),"evidence_sha256":crate::measure::sha256(&text)}));
        }
        comparisons.push(json!({"experiment_sha256":experiment.key,"components":components}));
    }
    let _preparation = crate::admission::functional("title-drift memory record validation", budget)
        .map_err(|f| {
            f.msg
                .unwrap_or_else(|| "functional validation admission refused".into())
        })?;
    let document = json!({"footprint_semantics":FOOTPRINT_SEMANTICS,"kind":"functional_memory_sizing","timing_qualified":false,"comparisons":comparisons});
    // Validate through the production reader before publishing the reusable record.
    let draft = o.out.join("memory-record-draft.json");
    crate::util::write_fresh(&draft, document.to_string()).map_err(|e| e.to_string())?;
    for (index, experiment) in experiments.iter().enumerate() {
        experiment.validate()?;
        crate::timing_comparison::Memory::read(draft.clone(), experiment)?.recheck()?;
        let member = capture::controller_snapshot()?;
        let diagnostic = json!({"footprint_semantics":FOOTPRINT_SEMANTICS,"kind":"functional_memory_preparation","timing_qualified":false,
            "functional_budget_bytes":budget,"ordinary_spare_bytes":crate::admission::FUNCTIONAL_MARGIN,
            "controller":member});
        crate::util::write_fresh(
            &o.out.join(format!("preparation-{index}.json")),
            diagnostic.to_string(),
        )
        .map_err(|e| e.to_string())?;
        preparation_fits(&member, budget, &comparisons)?;
    }
    std::fs::rename(&draft, o.out.join("memory-record.json")).map_err(|e| e.to_string())
}

fn preparation_fits(member: &Value, budget: u64, comparisons: &[Value]) -> Result<(), String> {
    let peak = FootprintObservation::read(member)?.envelope();
    if peak > budget {
        return Err(format!(
            "actual preparation controller peak {peak} exceeds explicit functional budget {budget}"
        ));
    }
    for comparison in comparisons {
        let bound = comparison
            .get("components")
            .and_then(Value::as_array)
            .and_then(|v| {
                v.iter()
                    .find(|v| v.get("name").and_then(Value::as_str) == Some("controller"))
            })
            .and_then(|v| v.get("bound_bytes"))
            .and_then(Value::as_u64)
            .ok_or("missing prepared controller bound")?;
        if peak > bound {
            return Err(format!(
                "actual preparation controller peak {peak} exceeds controller bound {bound}; explicit growth does not cover parsing"
            ));
        }
    }
    Ok(())
}

fn bound(peak: u64, growth: u64) -> Result<u64, String> {
    if peak == 0 || growth == 0 {
        return Err("sizing peak and explicit growth must be positive".into());
    }
    peak.checked_add(peak.div_ceil(5))
        .and_then(|v| v.checked_add(growth))
        .ok_or_else(|| "sizing bound overflow".into())
}

fn sizing_take(
    o: &Options,
    case: Case,
    build: &Build,
    experiment: &crate::timing_comparison::Experiment,
    budget: u64,
    stem: &std::path::Path,
    raw: &mut Raw,
) -> Result<(capture::Report, String), String> {
    experiment.validate()?;
    let (mut world, files, launch) = fixture_world(build, &o.work, o.keep, raw)?;
    let generation = crate::band::Generation::fresh()?;
    let word = generation.word().to_owned();
    let (controller, digest) = experiment.controller();
    let mut recorder = None;
    let started = Instance::start_functional_sizing(&world, launch, budget, generation, || {
        recorder = Some(capture::Capture::start(
            stem.with_extension("memory.jsonl"),
            experiment.key.clone(),
            word.clone(),
            build.clone(),
            controller.to_owned(),
            digest.to_owned(),
            budget,
        )?);
        Ok(())
    });
    let mut inst = match started {
        Ok(inst) => inst,
        Err(why) => {
            world.keep();
            return Err(why);
        }
    };
    let recorder = recorder.ok_or("functional sizing recorder was not admitted")?;
    let fixture = (|| {
        inst.await_socket(instance::BOOT)
            .map_err(|e| e.msg.unwrap_or_else(|| "sizing boot failed".into()))?;
        if inst.comparison_generation() != Some(word.as_str()) || inst.timing_admission().is_ok() {
            return Err(
                "sizing generation changed or functional instance gained timing authority".into(),
            );
        }
        recorder.bind(&inst, build)?;
        // Producers do not exist until setup; the first whole-tree checkpoint follows it.
        recorder.phase("setup")?;
        let prepared = prepare_fixture(&inst, build, case, &o.work, &files, raw)?;
        recorder.checkpoint()?;
        recorder.phase("fixture")?;
        exercise_fixture(&inst, build, case, &o.work, &files, prepared, raw)?;
        recorder.checkpoint()?;
        experiment.validate()?;
        recorder.phase("before_cleanup")?;
        recorder.checkpoint()?;
        Ok(())
    })();
    // Even an earlier fixture failure must perform and record exact process cleanup.
    let phase = recorder.phase("cleanup");
    let result = finish(
        fixture.and(phase),
        || {
            inst.stop()?;
            // A fast teardown cannot skip the positive final controller/retirement sweep.
            recorder.checkpoint()
        },
        raw,
    );
    let report = recorder.finish();
    let result = result.and(report);
    if result.is_err() {
        world.keep();
    }
    result.map(|report| (report, word))
}

#[cfg(test)]
#[path = "title_drift_calibration_tests.rs"]
mod tests;
