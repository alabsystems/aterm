// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn experiment() -> Value {
    let takes = ["before", "after", "after", "before"]
        .map(|arm| json!({"source":"candidate","slots":4,"arm":arm,"expected_red":false}));
    json!({
        "context":{"source":"candidate","slots":4,"controller_sha256":"controller"},
        "takes":takes,
    })
}

fn root(parent: &Path, name: &str) -> PathBuf {
    let root = parent.join(name);
    std::fs::create_dir(&root).unwrap();
    root
}

fn put(value: &mut Value, key: &str, entry: Value) {
    value.as_object_mut().unwrap().insert(key.into(), entry);
}

fn result(spec: &Value, index: usize) -> Value {
    let recipe = &spec["takes"][index];
    let red = recipe["expected_red"].as_bool().unwrap();
    let hash = "0".repeat(64);
    let mut value = json!({
        "appkit":{"success":true}, "export":{"success":true,"staging_push":false},
        "fixtures":{"success":true},
        "tree":[{"success":!red,"expected_red":red}],
        "serial_export":if red {Value::Null} else {json!({"success":true,"staging_push":false})},
        "serial_staging_push_s":null,
        "planted_red":red.then_some("planted-red.json"),
        "planted_failure_marker_s":red.then_some(1.0),
        "build_child":{"postlink":{
            "success":true,"kind":"real-unsigned-build-finishing",
            "commit":"candidate","architectures":"arm64 x86_64",
            "snapshot_digest":hash,"macho_sha256":hash,"bundle_digest":hash,
            "content_digest":hash,"inputs":hash,
        }},
    });
    for (key, entry) in recipe.as_object().unwrap() {
        if key != "expected_red" {
            put(&mut value, key, entry.clone());
        }
    }
    value
}

fn write_result(root: &Path, spec: &Value, index: usize, value: &Value) -> PathBuf {
    let at = take_dir(root, index, &spec["takes"][index]).unwrap();
    std::fs::create_dir_all(&at).unwrap();
    crate::durable::write_fresh(&at.join("result.json"), value.to_string()).unwrap();
    at
}

#[test]
fn a_failed_partial_take_is_not_completed_or_retried_by_resume() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let first = root(temp.path(), "first");
    let spec = experiment();
    let plan = Plan::prepare(&first, spec.clone(), None, None, "{\"seed\":1}".into()).unwrap();
    let mut failed = result(&spec, 0);
    put(
        &mut failed,
        "serial_export",
        json!({"success":false,"staging_push":false}),
    );
    let at = write_result(&first, &spec, 0, &failed);
    assert!(plan.complete(0, &at).is_err());
    assert!(!at.join(COMPLETE).exists());

    let second = root(temp.path(), "second");
    let resumed = Plan::prepare(&second, spec.clone(), Some(&first), None, "{}".into()).unwrap();
    assert_eq!(resumed.next, 0);
    assert_eq!(resumed.history, "{\"seed\":1}");
    assert!(!take_dir(&second, 0, &spec["takes"][0]).unwrap().exists());
}

#[test]
fn validated_prefixes_continue_in_order_with_the_original_seed() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let spec = experiment();
    let first = root(temp.path(), "first");
    let plan = Plan::prepare(&first, spec.clone(), None, None, "{\"seed\":1}".into()).unwrap();
    let at = write_result(&first, &spec, 0, &result(&spec, 0));
    plan.complete(0, &at).unwrap();

    let second = root(temp.path(), "second");
    let plan = Plan::prepare(
        &second,
        spec.clone(),
        Some(&first),
        None,
        "{\"seed\":2}".into(),
    )
    .unwrap();
    assert_eq!(plan.next, 1);
    assert_eq!(plan.history, "{\"seed\":1}");
    let at = write_result(&second, &spec, 1, &result(&spec, 1));
    plan.complete(1, &at).unwrap();

    let third = root(temp.path(), "third");
    let plan = Plan::prepare(&third, spec.clone(), Some(&second), None, "{}".into()).unwrap();
    assert_eq!(plan.next, 2);
    for index in 2..4 {
        let at = write_result(&third, &spec, index, &result(&spec, index));
        plan.complete(index, &at).unwrap();
    }
    let fourth = root(temp.path(), "fourth");
    assert!(Plan::prepare(&fourth, spec, Some(&third), None, "{}".into()).is_err());
}

#[test]
fn changed_results_seeds_and_core_contexts_cannot_be_resumed() {
    for mutation in ["result", "seed", "context"] {
        let temp = aterm_tempfile::tempdir().unwrap();
        let first = root(temp.path(), "first");
        let mut spec = experiment();
        let plan = Plan::prepare(&first, spec.clone(), None, None, "{}".into()).unwrap();
        let at = write_result(&first, &spec, 0, &result(&spec, 0));
        plan.complete(0, &at).unwrap();
        match mutation {
            "result" => {
                let mut changed = result(&spec, 0);
                put(&mut changed, "wall_s", json!(999));
                std::fs::write(at.join("result.json"), changed.to_string()).unwrap();
            }
            "seed" => std::fs::write(first.join(HISTORY), "{\"changed\":true}").unwrap(),
            _ => put(
                spec.as_object_mut().unwrap().get_mut("context").unwrap(),
                "slots",
                json!(8),
            ),
        }
        let next = root(temp.path(), "next");
        assert!(Plan::prepare(&next, spec, Some(&first), None, "{}".into()).is_err());
    }
}

#[test]
fn a_gap_or_cycle_in_completion_lineage_is_refused() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let first = root(temp.path(), "first");
    let spec = experiment();
    let plan = Plan::prepare(&first, spec.clone(), None, None, "{}".into()).unwrap();
    let at = write_result(&first, &spec, 1, &result(&spec, 1));
    plan.complete(1, &at).unwrap();
    let next = root(temp.path(), "next");
    assert!(Plan::prepare(&next, spec.clone(), Some(&first), None, "{}".into()).is_err());

    let mut manifest = read_manifest(&first).unwrap();
    put(&mut manifest, "parent", json!(first));
    std::fs::write(first.join(MANIFEST), manifest.to_string()).unwrap();
    assert!(Plan::prepare(&next, spec, Some(&first), None, "{}".into()).is_err());
}

#[test]
fn a_self_consistent_replacement_seed_cannot_relabel_an_existing_prefix() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let first = root(temp.path(), "first");
    let spec = experiment();
    let plan = Plan::prepare(&first, spec.clone(), None, None, "{}".into()).unwrap();
    let at = write_result(&first, &spec, 0, &result(&spec, 0));
    plan.complete(0, &at).unwrap();
    std::fs::write(first.join(HISTORY), "{\"different\":true}").unwrap();
    let mut manifest = read_manifest(&first).unwrap();
    put(
        &mut manifest,
        "history_sha256",
        json!(digest(&first.join(HISTORY)).unwrap()),
    );
    std::fs::write(first.join(MANIFEST), manifest.to_string()).unwrap();
    let next = root(temp.path(), "next");
    assert!(Plan::prepare(&next, spec, Some(&first), None, "{}".into()).is_err());
}

#[test]
fn green_and_red_series_can_share_a_seed_without_sharing_completion() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let first = root(temp.path(), "green");
    let mut spec = experiment();
    let _ = Plan::prepare(&first, spec.clone(), None, None, "{\"seed\":1}".into()).unwrap();
    for recipe in spec
        .as_object_mut()
        .unwrap()
        .get_mut("takes")
        .unwrap()
        .as_array_mut()
        .unwrap()
    {
        put(recipe, "expected_red", json!(true));
    }
    let second = root(temp.path(), "red");
    let plan = Plan::prepare(&second, spec.clone(), None, Some(&first), "{}".into()).unwrap();
    assert_eq!(plan.next, 0);
    assert_eq!(plan.history, "{\"seed\":1}");
    let at = write_result(&second, &spec, 0, &result(&spec, 0));
    plan.complete(0, &at).unwrap();
    let next = root(temp.path(), "next");
    assert!(Plan::prepare(&next, experiment(), Some(&second), None, "{}".into()).is_err());
}

#[test]
fn changing_a_verifier_changes_the_bound_experiment() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let verify = temp.path().join("verifier");
    std::fs::write(&verify, "first image").unwrap();
    let opts = super::super::parse(
        [
            "--verify",
            verify.to_str().unwrap(),
            "--before-verify",
            verify.to_str().unwrap(),
            "--mode",
            "matched",
            "--tree-work",
            "release",
            "--export-work",
            "real",
            "--queued-tree-share",
            "historical-lend",
            "--cores",
            "4",
        ]
        .into_iter()
        .map(str::to_owned),
    )
    .unwrap();
    let original = spec(temp.path(), "candidate", &opts, 4, &opts.order()).unwrap();
    let first = root(temp.path(), "first");
    let _ = Plan::prepare(&first, original.clone(), None, None, "{}".into()).unwrap();
    std::fs::write(&verify, "second image").unwrap();
    let changed = spec(temp.path(), "candidate", &opts, 4, &opts.order()).unwrap();
    assert_ne!(original, changed);
    let next = root(temp.path(), "next");
    assert!(Plan::prepare(&next, changed, Some(&first), None, "{}".into()).is_err());
}
