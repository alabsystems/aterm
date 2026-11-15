// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn seal() -> Seal {
    Seal {
        reservation: "reservation-1".into(),
        ordinal: 0,
        generation: "arm-1".into(),
        path: "/frozen/app".into(),
        source: "source-1".into(),
        digest: "digest-1".into(),
        record: "/frozen/build.jsonl".into(),
        record_digest: "record-1".into(),
        artifact: "artifact-1".into(),
        identity: aterm_uds::process::read_identity(std::process::id() as i32).unwrap(),
        keeper: aterm_uds::process::read_identity(std::process::id() as i32).unwrap(),
        keeper_path: "/keeper".into(),
        controller_digest: "controller-1".into(),
    }
}
fn running() -> Boundary {
    let mut boundary = Boundary::default();
    boundary.bind(seal()).unwrap();
    boundary.entered = true;
    boundary
}

#[test]
fn actual_failed_final_is_terminal_and_quiet_peer_or_later_arm_cannot_rescue_it() {
    for failure in [
        "actual CPU busy",
        "missing GPU",
        "stale previous-arm observation",
    ] {
        let mut boundary = running();
        let mut calls = 0;
        let actual_final = boundary.final_sample(|| {
            calls += 1;
            Err::<(), _>(failure.to_owned())
        });
        assert_eq!(actual_final, Err(failure.to_owned()));
        // Even a genuinely quiet second observation is never requested after the first
        // actual final refusal. This exercises the same transition called by Arm::run.
        assert!(
            boundary
                .final_sample(|| {
                    calls += 1;
                    Ok(())
                })
                .is_err()
        );
        assert_eq!(calls, 1);
        assert!(boundary.complete().is_err());
        // Mutation control: omitting the real final check and substituting a quiet
        // peer makes the rejected trace pass. The production assertion above detects it.
        let mut omitted_real_check = running();
        omitted_real_check.final_sample(|| Ok(())).unwrap();
        assert!(omitted_real_check.complete().is_ok());
    }
}

#[test]
fn every_live_binding_is_exact_and_cross_arm_or_peer_receipts_are_rejected() {
    let expected = seal();
    let mutations: &[fn(&mut Seal)] = &[
        |s| s.reservation.push('2'),
        |s| s.ordinal += 1,
        |s| s.generation.push('2'),
        |s| s.path.push("other"),
        |s| s.source.push('2'),
        |s| s.digest.push('2'),
        |s| s.record.push("other"),
        |s| s.record_digest.push('2'),
        |s| s.artifact.push('2'),
        |s| s.keeper.pid += 1,
        |s| s.keeper.birth.0 += 1,
        |s| s.keeper.idversion = Some(s.keeper.idversion.unwrap_or(0) + 1),
        |s| s.keeper_path.push("other"),
        |s| s.controller_digest.push('2'),
        |s| s.identity.pid += 1,
        |s| s.identity.idversion = Some(s.identity.idversion.unwrap_or(0) + 1),
        |s| s.identity.birth.0 += 1,
    ];
    for mutate in mutations {
        let mut boundary = Boundary::default();
        boundary.bind(expected.clone()).unwrap();
        let mut actual = expected.clone();
        mutate(&mut actual);
        assert_ne!(expected, actual);
        assert!(boundary.bind(actual).is_err());
    }
}

#[test]
fn only_four_clean_qualified_sequential_arms_complete_one_quad() {
    let mut progress = Progress::default();
    for arm in crate::aos_cadence::ORDER {
        progress.start(true, arm).unwrap();
        assert!(progress.complete().is_err());
        let mut boundary = running();
        boundary.final_sample(|| Ok(())).unwrap();
        boundary.complete().unwrap();
        progress.cleaned().unwrap();
    }
    progress.complete().unwrap();
    assert!(
        progress.start(true, 'A').is_err(),
        "one reservation cannot start a fifth arm"
    );
    for (same_case, arm) in [(false, 'A'), (true, 'B')] {
        let mut progress = Progress::default();
        assert!(progress.start(same_case, arm).is_err());
        assert!(progress.start(true, 'A').is_err());
    }
    let mut overlapping = Progress::default();
    overlapping.start(true, 'A').unwrap();
    assert!(overlapping.start(true, 'A').is_err());
    assert!(overlapping.complete().is_err());
    // Cleanup loss never invokes cleaned; the reservation remains poisoned.
    let mut cleanup_lost = Progress::default();
    cleanup_lost.start(true, 'A').unwrap();
    assert!(cleanup_lost.start(true, 'B').is_err());
    assert!(cleanup_lost.complete().is_err());
}

#[test]
fn real_cleanup_loss_expiry_and_duplicate_completion_cancel_the_quad() {
    for cleanup_ok in [false, true] {
        let mut progress = Progress::default();
        progress.start(true, 'A').unwrap();
        let mut boundary = running();
        boundary.final_sample(|| Ok(())).unwrap();
        let mut raw = Raw::default();
        let mut qualified = false;
        let result = complete_after_cleanup(
            &mut progress,
            &boundary,
            Ok(7),
            || {
                if cleanup_ok {
                    Ok(())
                } else {
                    Err("exact cleanup lost".into())
                }
            },
            || {
                qualified = true;
                Err("original claim expired during cleanup".into())
            },
            &mut raw,
        );
        assert!(result.is_err());
        assert_eq!(
            qualified, cleanup_ok,
            "cleanup refusal never reaches qualification"
        );
        assert_eq!(progress.next, 0);
        assert_eq!(progress.state, QuadState::Cancelled);
        assert!(progress.start(true, 'A').is_err());
        assert!(progress.complete().is_err());
        assert!(raw.as_text().contains("cleanup"));
    }
    let mut progress = Progress::default();
    progress.start(true, 'A').unwrap();
    let mut boundary = running();
    boundary.final_sample(|| Ok(())).unwrap();
    let mut raw = Raw::default();
    assert_eq!(
        complete_after_cleanup(
            &mut progress,
            &boundary,
            Ok(7),
            || Ok(()),
            || Ok(()),
            &mut raw
        )
        .unwrap(),
        7
    );
    assert_eq!(progress.next, 1);
    assert!(
        complete_after_cleanup(
            &mut progress,
            &boundary,
            Ok(7),
            || Ok(()),
            || Ok(()),
            &mut raw
        )
        .is_err()
    );
    assert_eq!(progress.next, 1);
    assert!(progress.start(true, 'B').is_err());
    let mut overlapping = Progress::default();
    overlapping.start(true, 'A').unwrap();
    assert!(overlapping.start(true, 'A').is_err());
    assert!(
        overlapping.cleaned().is_err(),
        "a canceled overlap cannot be repaired by later cleanup"
    );
}

#[test]
fn missing_entry_and_duplicate_completion_have_no_qualified_witness() {
    let mut no_entry = Boundary::default();
    no_entry.bind(seal()).unwrap();
    assert!(no_entry.final_sample(|| Ok(())).is_err());
    assert!(no_entry.complete().is_err());
    let mut boundary = running();
    assert!(boundary.can_enter().is_err());
    boundary.final_sample(|| Ok(())).unwrap();
    assert!(boundary.final_sample(|| Ok(())).is_err());
}

#[test]
fn frozen_executable_and_actual_compiler_record_bytes_are_rechecked() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("app");
    let record = dir.path().join("build.jsonl");
    std::fs::write(&path, "frozen app").unwrap();
    std::fs::write(&record, "frozen record").unwrap();
    let build = Build {
        path: path.clone(),
        source: "a".repeat(40),
        digest: aterm_update_core::sha256_file(&path).unwrap(),
        record: record.clone(),
        record_digest: aterm_update_core::sha256_file(&record).unwrap(),
        artifact: "artifact".into(),
    };
    check_file(&build).unwrap();
    std::fs::write(&path, "changed app").unwrap();
    assert!(check_file(&build).is_err());
    std::fs::write(&path, "frozen app").unwrap();
    std::fs::write(&record, "changed record").unwrap();
    assert!(check_file(&build).is_err());
}

fn build_pair(dir: &std::path::Path) -> [Build; 2] {
    std::array::from_fn(|i| Build {
        path: dir.join(format!("app-{i}")),
        source: format!("{i}").repeat(40),
        digest: format!("{i}").repeat(64),
        record: dir.join(format!("record-{i}")),
        record_digest: format!("{}", i + 2).repeat(64),
        artifact: format!("artifact-{i}"),
    })
}

fn experiment_fixture(builds: &[Build; 2], key: &str) -> Experiment {
    Experiment {
        builds: builds.clone(),
        case: "1".into(),
        key: key.into(),
        controller_digest: "controller".into(),
        controller_path: "/controller".into(),
        quads: 1,
    }
}

fn at_mut<'a>(value: &'a mut aterm_json::Value, path: &[&str]) -> &'a mut aterm_json::Value {
    let mut current = value;
    for key in path {
        current = if let Ok(index) = key.parse::<usize>() {
            &mut current.as_array_mut().unwrap()[index]
        } else {
            current.as_object_mut().unwrap().get_mut(*key).unwrap()
        };
    }
    current
}

fn measurements(component: &str, builds: &[Build; 2]) -> aterm_json::Value {
    let identity = aterm_uds::process::read_identity(std::process::id() as i32).unwrap();
    let observations: Vec<_> = builds.iter().enumerate().map(|(i, build)| {
        let mut app_identity = identity;
        app_identity.pid += i as i32 + 1;
        let mut keeper_identity = identity;
        keeper_identity.pid += i as i32 + 10;
        let controller = aterm_json::json!({"identity":keeper_identity.token(),"executable":"/controller","binary_sha256":"controller","footprint_semantics":FOOTPRINT_SEMANTICS,"footprint_bytes":100 + i,"lifetime_peak_footprint_bytes":99 + i});
        let members = if component == "app_and_keeper" {
            vec![aterm_json::json!({"identity":app_identity.token(),"footprint_semantics":FOOTPRINT_SEMANTICS,"footprint_bytes":100 + i,"lifetime_peak_footprint_bytes":99 + i,
                "executable":build.path.display().to_string(),"binary_sha256":build.digest,
                "source_sha":build.source,"build_record_sha256":build.record_digest}), controller]
        } else { vec![controller] };
        aterm_json::json!({"monotonic_ns":i + 1,"members":members})
    }).collect();
    aterm_json::json!({"footprint_semantics":FOOTPRINT_SEMANTICS,"experiment_sha256":"experiment","component":component,"observations":observations})
}

#[test]
fn memory_peak_is_recomputed_from_bound_machine_readings_and_both_frozen_builds() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let builds = build_pair(dir.path());
    let good = measurements("app_and_keeper", &builds);
    assert_eq!(
        measured_peak(
            &good.to_string(),
            &experiment_fixture(&builds, "experiment"),
            "app_and_keeper"
        )
        .unwrap(),
        202
    );
    for key in [
        "footprint_bytes",
        "identity",
        "binary_sha256",
        "source_sha",
        "build_record_sha256",
        "executable",
    ] {
        let mut bad = good.clone();
        *at_mut(&mut bad, &["observations", "0", "members", "0", key]) = aterm_json::Value::Null;
        assert!(
            measured_peak(
                &bad.to_string(),
                &experiment_fixture(&builds, "experiment"),
                "app_and_keeper"
            )
            .is_err(),
            "{key}"
        );
    }
    assert!(
        measured_peak(
            &good.to_string(),
            &experiment_fixture(&builds, "different case/work"),
            "app_and_keeper"
        )
        .is_err()
    );
    let mut bad = good.clone();
    *at_mut(&mut bad, &["observations", "1", "monotonic_ns"]) = aterm_json::json!(1);
    assert!(
        measured_peak(
            &bad.to_string(),
            &experiment_fixture(&builds, "experiment"),
            "app_and_keeper"
        )
        .is_err()
    );
    let mut bad = good.clone();
    *at_mut(
        &mut bad,
        &["observations", "0", "members", "0", "footprint_bytes"],
    ) = aterm_json::json!(0);
    assert!(
        measured_peak(
            &bad.to_string(),
            &experiment_fixture(&builds, "experiment"),
            "app_and_keeper"
        )
        .is_err()
    );
    let mut missing_keeper = good.clone();
    at_mut(&mut missing_keeper, &["observations", "0", "members"])
        .as_array_mut()
        .unwrap()
        .pop();
    assert!(
        measured_peak(
            &missing_keeper.to_string(),
            &experiment_fixture(&builds, "experiment"),
            "app_and_keeper"
        )
        .is_err()
    );
    let mut reused_identity = good.clone();
    *at_mut(
        &mut reused_identity,
        &["observations", "1", "members", "0", "identity"],
    ) = good["observations"][0]["members"][0]["identity"].clone();
    assert!(
        measured_peak(
            &reused_identity.to_string(),
            &experiment_fixture(&builds, "experiment"),
            "app_and_keeper"
        )
        .is_err()
    );
}

#[test]
fn memory_record_refuses_nonpositive_insufficient_unknown_and_unbound_components() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let builds = build_pair(dir.path());
    let names = [
        "controller",
        "app_and_keeper",
        "producers",
        "setup_cleanup_overlap",
    ];
    let mut captures = Vec::new();
    let mut collected: std::collections::BTreeMap<String, Vec<aterm_json::Value>> =
        std::collections::BTreeMap::new();
    for (ordinal, arm) in crate::aos_cadence::ORDER.iter().enumerate() {
        let build_index = usize::from(*arm == 'B');
        let build = &builds[build_index];
        let generation = format!("{ordinal:032x}");
        let mut witnesses = aterm_json::json!({});
        let mut roles = aterm_json::json!({});
        for name in names {
            let mut observation = measurements(name, &builds)["observations"][build_index].clone();
            let object = observation.as_object_mut().unwrap();
            object.insert("monotonic_ns".into(), aterm_json::json!(ordinal * 3 + 1));
            object.insert("phase".into(), aterm_json::json!("fixture"));
            object.insert("capture_generation".into(), aterm_json::json!(generation));
            roles
                .as_object_mut()
                .unwrap()
                .insert(name.into(), observation["members"].clone());
            witnesses
                .as_object_mut()
                .unwrap()
                .insert(name.into(), aterm_json::json!([observation]));
            collected.entry(name.into()).or_default().push(observation);
        }
        let raw = dir.path().join(format!("raw-{ordinal}.jsonl"));
        let header = aterm_json::json!({"footprint_semantics":FOOTPRINT_SEMANTICS,"kind":"functional_memory_sizing","experiment_sha256":"experiment",
            "generation":generation,"functional_budget_bytes":4096,"timing_qualified":false});
        let controller = roles["controller"][0]["identity"].as_str().unwrap();
        let mut originals = sizing_live_identities(&roles).unwrap();
        originals.remove(controller);
        let mut originals: Vec<_> = originals.into_iter().collect();
        originals.sort();
        let sweep = aterm_json::json!({"event":"sweep","phase":"fixture","monotonic_ns":ordinal*3+1,"components":roles,
            "original_non_controller_identities":originals,"retired_original_identities":[]});
        let cleanup = aterm_json::json!({"event":"sweep","phase":"cleanup","monotonic_ns":ordinal*3+2,"components":{"controller":roles["controller"]},
            "original_non_controller_identities":originals,"retired_original_identities":originals});
        let complete = aterm_json::json!({"event":"sweep","phase":"cleanup_complete","monotonic_ns":ordinal*3+3,"components":{"controller":roles["controller"]},
            "original_non_controller_identities":originals,"retired_original_identities":originals});
        let stopped =
            aterm_json::json!({"event":"recorder_stop","error":null,"cleanup_complete":true});
        std::fs::write(
            &raw,
            format!("{header}\n{sweep}\n{cleanup}\n{complete}\n{stopped}\n"),
        )
        .unwrap();
        let fixture_raw = dir.path().join(format!("fixture-{ordinal}.txt"));
        let cast = dir.path().join(format!("fixture-{ordinal}.cast"));
        std::fs::write(
            &fixture_raw,
            "synthetic fixture and exact cleanup diagnostics",
        )
        .unwrap();
        std::fs::write(&cast, "synthetic injected cast").unwrap();
        let path = dir.path().join(format!("capture-{ordinal}.json"));
        let capture = aterm_json::json!({"footprint_semantics":FOOTPRINT_SEMANTICS,"kind":"functional_memory_sizing","experiment_sha256":"experiment",
            "case":"1","ordinal":ordinal,"arm":arm.to_string(),"binary_sha256":build.digest,"source_sha":build.source,
            "build_record_sha256":build.record_digest,"fixture_complete":true,"cleanup_complete":true,"timing_qualified":false,
            "functional_budget_bytes":4096,"generation":generation,"raw_path":raw.display().to_string(),
            "raw_sha256":aterm_update_core::sha256_file(&raw).unwrap(),
            "fixture_raw_path":fixture_raw.display().to_string(),"fixture_raw_sha256":aterm_update_core::sha256_file(&fixture_raw).unwrap(),
            "cast_path":cast.display().to_string(),"cast_sha256":aterm_update_core::sha256_file(&cast).unwrap(),"witnesses":witnesses});
        std::fs::write(&path, capture.to_string()).unwrap();
        captures.push(aterm_json::json!({"path":path.display().to_string(),"sha256":aterm_update_core::sha256_file(&path).unwrap()}));
    }
    let mut components = Vec::new();
    for name in names {
        let path = dir.path().join(format!("{name}.json"));
        let evidence = aterm_json::json!({"footprint_semantics":FOOTPRINT_SEMANTICS,"experiment_sha256":"experiment","component":name,"observations":collected[name],"captures":captures});
        std::fs::write(&path, evidence.to_string()).unwrap();
        components.push(aterm_json::json!({"name":name, "bound_bytes":1024,
            "bound_derivation":"synthetic test envelope over injected 100/101-byte member footprints",
            "evidence_path": path.display().to_string(), "evidence_sha256":aterm_update_core::sha256_file(&path).unwrap()}));
    }
    let document = aterm_json::json!({"footprint_semantics":FOOTPRINT_SEMANTICS,"comparisons":[{"experiment_sha256":"experiment", "components":components}]});
    let record = dir.path().join("memory.json");
    std::fs::write(&record, document.to_string()).unwrap();
    assert_eq!(
        Memory::read(record.clone(), &experiment_fixture(&builds, "experiment"))
            .unwrap()
            .bytes,
        4096
    );
    for semantics in [
        aterm_json::Value::Null,
        aterm_json::json!("old_atomic_pair"),
    ] {
        let mut legacy = document.clone();
        *at_mut(&mut legacy, &["footprint_semantics"]) = semantics;
        std::fs::write(&record, legacy.to_string()).unwrap();
        assert!(Memory::read(record.clone(), &experiment_fixture(&builds, "experiment")).is_err());
    }
    std::fs::write(&record, document.to_string()).unwrap();
    let memory = Memory::read(record.clone(), &experiment_fixture(&builds, "experiment")).unwrap();
    let originals: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let bytes = std::fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect();
    // Keep the hash graph internally consistent while hiding a real larger cleanup
    // sweep from the selected witnesses. A registry of hashes alone would accept it.
    let raw = dir.path().join("raw-0.jsonl");
    let mut rows: Vec<aterm_json::Value> = std::fs::read_to_string(&raw)
        .unwrap()
        .lines()
        .map(|line| aterm_json::from_str(line).unwrap())
        .collect();
    for key in ["footprint_bytes", "lifetime_peak_footprint_bytes"] {
        *at_mut(&mut rows[2], &["components", "controller", "0", key]) = aterm_json::json!(300);
        *at_mut(&mut rows[3], &["components", "controller", "0", key]) = aterm_json::json!(300);
    }
    std::fs::write(
        &raw,
        rows.iter().map(|v| format!("{v}\n")).collect::<String>(),
    )
    .unwrap();
    let mut hidden_peak = document.clone();
    for component in at_mut(&mut hidden_peak, &["comparisons", "0", "components"])
        .as_array_mut()
        .unwrap()
    {
        let evidence_path = component["evidence_path"].as_str().unwrap().to_owned();
        let mut evidence: aterm_json::Value =
            aterm_json::from_str(&std::fs::read_to_string(&evidence_path).unwrap()).unwrap();
        for reference in evidence
            .as_object_mut()
            .unwrap()
            .get_mut("captures")
            .unwrap()
            .as_array_mut()
            .unwrap()
        {
            let capture_path = reference["path"].as_str().unwrap().to_owned();
            let mut capture: aterm_json::Value =
                aterm_json::from_str(&std::fs::read_to_string(&capture_path).unwrap()).unwrap();
            let raw_path = std::path::PathBuf::from(capture["raw_path"].as_str().unwrap());
            capture.as_object_mut().unwrap().insert(
                "raw_sha256".into(),
                aterm_json::json!(aterm_update_core::sha256_file(&raw_path).unwrap()),
            );
            std::fs::write(&capture_path, capture.to_string()).unwrap();
            reference.as_object_mut().unwrap().insert(
                "sha256".into(),
                aterm_json::json!(
                    aterm_update_core::sha256_file(std::path::Path::new(&capture_path)).unwrap()
                ),
            );
        }
        std::fs::write(&evidence_path, evidence.to_string()).unwrap();
        component.as_object_mut().unwrap().insert(
            "evidence_sha256".into(),
            aterm_json::json!(
                aterm_update_core::sha256_file(std::path::Path::new(&evidence_path)).unwrap()
            ),
        );
    }
    std::fs::write(&record, hidden_peak.to_string()).unwrap();
    let refused = Memory::read(record.clone(), &experiment_fixture(&builds, "experiment"))
        .err()
        .unwrap();
    assert!(refused.contains("raw component peak"), "{refused}");
    for (path, bytes) in originals {
        std::fs::write(path, bytes).unwrap();
    }
    std::fs::write(dir.path().join("fixture-0.cast"), "changed cast").unwrap();
    assert!(memory.recheck().is_err());
    std::fs::write(dir.path().join("fixture-0.cast"), "synthetic injected cast").unwrap();
    memory.recheck().unwrap();
    assert!(
        Quad::reserve(&experiment_fixture(&builds, "other experiment"), &memory).is_err(),
        "mismatched evidence refuses before any account/window/resource admission"
    );
    for value in [
        aterm_json::Value::Null,
        aterm_json::json!(0),
        aterm_json::json!(-1),
        aterm_json::json!(100),
        aterm_json::json!(101),
        aterm_json::json!(102),
        aterm_json::json!(121),
    ] {
        let mut bad = document.clone();
        *at_mut(
            &mut bad,
            &["comparisons", "0", "components", "0", "bound_bytes"],
        ) = value;
        std::fs::write(&record, bad.to_string()).unwrap();
        assert!(Memory::read(record.clone(), &experiment_fixture(&builds, "experiment")).is_err());
    }
    std::fs::write(&record, document.to_string()).unwrap();
    assert!(
        Memory::read(
            record.clone(),
            &experiment_fixture(&builds, "wrong experiment")
        )
        .is_err()
    );
    std::fs::write(
        dir.path().join("producers.json"),
        "arbitrary nonempty prose is not a footprint reading",
    )
    .unwrap();
    assert!(Memory::read(record, &experiment_fixture(&builds, "experiment")).is_err());
}

#[test]
fn actual_boundary_transitions_refine_the_final_decision_model() {
    use aterm_spec::interp::{State, with_buggy};
    let model = aterm_models_capacity::timing_comparison_final_model();
    let step = |model: &aterm_spec::derive::Model, state: &mut State, action| {
        let mut next = model.successors(action, state);
        assert_eq!(next.len(), 1, "{action}: {state:?}");
        *state = next.pop().unwrap();
    };
    let observe = |boundary: &Boundary, state: &State| {
        assert_eq!(i64::from(boundary.entered), i64::from(state["phase"] != 0));
        assert_eq!(i64::from(boundary.final_attempted), state["attempted"]);
        assert_eq!(i64::from(boundary.finalized), state["real_final"]);
        assert_eq!(boundary.complete().is_ok(), state["phase"] == 3);
    };
    for good in [false, true] {
        let mut state = model.init_state();
        let mut boundary = Boundary::default();
        boundary.bind(seal()).unwrap();
        observe(&boundary, &state);
        boundary.entered = true;
        step(&model, &mut state, "Entry");
        observe(&boundary, &state);
        step(&model, &mut state, "QuietPeer");
        observe(&boundary, &state);
        let actual_final = boundary.final_sample(|| {
            if good {
                Ok(())
            } else {
                Err("actual busy".into())
            }
        });
        assert_eq!(actual_final.is_ok(), good);
        step(
            &model,
            &mut state,
            if good { "QuietFinal" } else { "BadFinal" },
        );
        observe(&boundary, &state);
        assert!(boundary.final_sample(|| Ok(())).is_err());
        observe(&boundary, &state);
    }
    // Local negative controls match the production fault controls. The global registry
    // owns the full fault sweep; these two traces bind the omission/retry defects here.
    let omitted = with_buggy(&model, 1);
    let mut state = omitted.init_state();
    for action in ["Entry", "QuietPeer", "OmitActualFinal", "QualifiedCleanup"] {
        step(&omitted, &mut state, action);
    }
    assert_eq!(state["published"], 1);
    assert_eq!(state["real_final"], 0);
    let retried = with_buggy(&model, 2);
    let mut state = retried.init_state();
    for action in ["Entry", "BadFinal", "RetryRefusedFinal", "QualifiedCleanup"] {
        step(&retried, &mut state, action);
    }
    assert_eq!(state["published"], 1);
    assert_eq!(state["bad_seen"], 1);
}

#[test]
fn sizing_witness_requires_real_completed_raw_sweep_and_current_generation() {
    use aterm_json::json;
    let controller = aterm_uds::process::read_identity(std::process::id() as i32).unwrap();
    let mut child = controller;
    child.pid += 1;
    let capture = json!({"footprint_semantics":FOOTPRINT_SEMANTICS,"kind":"functional_memory_sizing","experiment_sha256":"experiment",
        "generation":"generation","functional_budget_bytes":4096,"timing_qualified":false});
    let member = json!({"identity":controller.token(),"footprint_semantics":FOOTPRINT_SEMANTICS,"footprint_bytes":100,"lifetime_peak_footprint_bytes":99});
    let child_member = json!({"identity":child.token(),"footprint_semantics":FOOTPRINT_SEMANTICS,"footprint_bytes":100,"lifetime_peak_footprint_bytes":99});
    let witness = json!({"monotonic_ns":1,"phase":"fixture","capture_generation":"generation","members":[member]});
    let sweep = json!({"event":"sweep","monotonic_ns":1,"phase":"fixture","components":{"controller":[member],"producers":[child_member]},
        "original_non_controller_identities":[child.token()],"retired_original_identities":[]});
    let cleanup = json!({"event":"sweep","monotonic_ns":2,"phase":"cleanup","components":sweep["components"],
        "original_non_controller_identities":[child.token()],"retired_original_identities":[]});
    let complete = json!({"event":"sweep","monotonic_ns":3,"phase":"cleanup_complete","components":{"controller":[member]},
        "original_non_controller_identities":[child.token()],"retired_original_identities":[child.token()]});
    let stop = json!({"event":"recorder_stop","error":null,"cleanup_complete":true});
    let raw = format!("{capture}\n{sweep}\n{cleanup}\n{complete}\n{stop}\n");
    assert!(
        validate_raw_witnesses(&raw, &capture, "controller", std::slice::from_ref(&witness))
            .is_ok()
    );
    for semantics in [aterm_json::Value::Null, json!("old_atomic_pair")] {
        let mut legacy = capture.clone();
        *at_mut(&mut legacy, &["footprint_semantics"]) = semantics;
        let stream = format!("{legacy}\n{sweep}\n{cleanup}\n{complete}\n{stop}\n");
        assert!(
            validate_raw_witnesses(
                &stream,
                &legacy,
                "controller",
                std::slice::from_ref(&witness)
            )
            .is_err()
        );
    }
    for raw in [
        format!("{capture}\n{sweep}\n"),
        format!("{capture}\n{sweep}\n{cleanup}\n{stop}\n"),
        format!("{capture}\n{sweep}\n{sweep}\n{complete}\n{stop}\n"),
    ] {
        assert!(
            validate_raw_witnesses(&raw, &capture, "controller", std::slice::from_ref(&witness))
                .is_err()
        );
    }
    for key in ["capture_generation", "members", "phase", "monotonic_ns"] {
        let mut bad = witness.clone();
        bad.as_object_mut()
            .unwrap()
            .insert(key.into(), aterm_json::Value::Null);
        assert!(validate_raw_witnesses(&raw, &capture, "controller", &[bad]).is_err());
    }
    // A still-live known producer is valid during draining, but cannot become a
    // successful final sweep even with a forged clean recorder_stop and new hashes.
    let mut survivor = complete.clone();
    survivor
        .as_object_mut()
        .unwrap()
        .insert("components".into(), sweep["components"].clone());
    survivor
        .as_object_mut()
        .unwrap()
        .insert("retired_original_identities".into(), json!([]));
    let false_complete = format!("{capture}\n{sweep}\n{cleanup}\n{survivor}\n{stop}\n");
    let refused = validate_raw_witnesses(
        &false_complete,
        &capture,
        "controller",
        std::slice::from_ref(&witness),
    )
    .err()
    .unwrap();
    assert!(refused.contains("surviving or unretired"), "{refused}");
    let mut forgotten = complete.clone();
    forgotten
        .as_object_mut()
        .unwrap()
        .insert("original_non_controller_identities".into(), json!([]));
    let forgotten = format!("{capture}\n{sweep}\n{cleanup}\n{forgotten}\n{stop}\n");
    assert!(validate_raw_witnesses(&forgotten, &capture, "controller", &[witness]).is_err());
}

#[test]
fn non_atomic_counters_preserve_both_independent_lower_bounds_in_production_reader() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let builds = build_pair(dir.path());
    let experiment = experiment_fixture(&builds, "experiment");
    let original = measurements("app_and_keeper", &builds);
    // The pair need not be ordered, at any difference. The same reader must also
    // retain a lifetime observation larger than the whole sampled current sum.
    for (current, lifetime, expected) in
        [(101, 100, 202), (1_000_000, 1, 2_000_000), (100, 900, 900)]
    {
        let mut evidence = original.clone();
        for observation in at_mut(&mut evidence, &["observations"])
            .as_array_mut()
            .unwrap()
        {
            for member in at_mut(observation, &["members"]).as_array_mut().unwrap() {
                *at_mut(member, &["footprint_bytes"]) = aterm_json::json!(current);
                *at_mut(member, &["lifetime_peak_footprint_bytes"]) = aterm_json::json!(lifetime);
            }
        }
        assert_eq!(
            measured_peak(&evidence.to_string(), &experiment, "app_and_keeper").unwrap(),
            expected
        );
        // Mutating either source of the envelope changes the actual production
        // result in the case where that source dominates. Ignoring it cannot pass.
        let key = if current > lifetime {
            "footprint_bytes"
        } else {
            "lifetime_peak_footprint_bytes"
        };
        let mut omitted = evidence.clone();
        for observation in at_mut(&mut omitted, &["observations"])
            .as_array_mut()
            .unwrap()
        {
            for member in at_mut(observation, &["members"]).as_array_mut().unwrap() {
                *at_mut(member, &[key]) = aterm_json::json!(1);
            }
        }
        assert!(
            measured_peak(&omitted.to_string(), &experiment, "app_and_keeper").unwrap() < expected
        );
        for key in [
            "footprint_bytes",
            "lifetime_peak_footprint_bytes",
            "footprint_semantics",
        ] {
            for bad in [
                aterm_json::Value::Null,
                aterm_json::json!(0),
                aterm_json::json!("unknown"),
            ] {
                let mut refused = evidence.clone();
                *at_mut(&mut refused, &["observations", "0", "members", "0", key]) = bad;
                assert!(
                    measured_peak(&refused.to_string(), &experiment, "app_and_keeper").is_err()
                );
            }
        }
    }
    for bad in [
        aterm_json::Value::Null,
        aterm_json::json!("old_atomic_pair"),
    ] {
        let mut evidence = original.clone();
        *at_mut(&mut evidence, &["footprint_semantics"]) = bad;
        assert!(measured_peak(&evidence.to_string(), &experiment, "app_and_keeper").is_err());
    }
    let mut overflow = original;
    for observation in at_mut(&mut overflow, &["observations"])
        .as_array_mut()
        .unwrap()
    {
        for member in at_mut(observation, &["members"]).as_array_mut().unwrap() {
            *at_mut(member, &["footprint_bytes"]) = aterm_json::json!(u64::MAX);
        }
    }
    assert!(measured_peak(&overflow.to_string(), &experiment, "app_and_keeper").is_err());
}
