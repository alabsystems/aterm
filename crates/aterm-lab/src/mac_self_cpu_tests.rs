// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn driver_requires_both_successful_exit_and_positive_production_evidence() {
    let proof = crate::self_cpu_qualification::tests::proof(9_000_000);
    let bytes = aterm_json::to_vec(&proof).unwrap();
    assert!(checked_output(&bytes, true).is_ok());
    assert!(checked_output(&bytes, false).is_err());
    assert!(checked_output(b"", true).is_err());
    assert!(checked_output(b"{}", true).is_err());
    let below =
        aterm_json::to_vec(&crate::self_cpu_qualification::tests::proof(7_999_999)).unwrap();
    assert!(checked_output(&below, true).is_err());
}

#[test]
fn artifact_selection_rejects_missing_duplicate_or_nonexistent_images() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let image = scratch.path().join(FIXTURE);
    std::fs::write(&image, b"never executed").unwrap();
    let artifact =
        json!({"reason":"compiler-artifact","target":{"name":FIXTURE},"executable":image});
    let line = aterm_json::to_string(&artifact).unwrap();
    assert_eq!(fixture_artifact(&line).unwrap(), image);
    assert!(fixture_artifact("").is_err());
    assert!(fixture_artifact(&format!("{line}\n{line}")).is_err());
    std::fs::remove_file(&image).unwrap();
    assert!(fixture_artifact(&line).is_err());
}

#[test]
fn driver_requires_an_explicit_record_and_unambiguous_options() {
    let args = |words: &[&str]| words.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    for words in [
        vec![],
        vec!["--image", "base"],
        vec!["--record"],
        vec!["--record", "a", "--record", "b"],
        vec!["--record", "a", "--image", "b", "--image", "c"],
        vec!["--record", "a", "--unknown", "b"],
    ] {
        assert!(parse(&args(&words)).is_err());
    }
    let ask = parse(&args(&[
        "--image",
        "isolated",
        "--record",
        "/tmp/.ubd/self-cpu-test.noindex",
    ]))
    .unwrap();
    assert_eq!(ask.image, "isolated");
    assert_eq!(ask.record, Path::new("/tmp/.ubd/self-cpu-test.noindex"));
}

#[test]
fn qualification_publication_requires_checked_vm_retirement() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let report = json!({"complete":true,"qualification":true,
        "checked_bound":{"retained_metric":"original"},"fixture_sha256":"exact-fixture"});
    let unavailable = finish_qualification(scratch.path(), Ok((0, report.clone())), || {
        assert!(!scratch.path().join("result.json").exists());
        Err(mac::not_run("checked retirement was unavailable"))
    });
    assert_eq!(unavailable.unwrap_err().code, mac::NOT_RUN);
    assert!(!scratch.path().join("result.json").exists());
    let observed: Value = aterm_json::from_slice(
        &std::fs::read(scratch.path().join("fixture-observation.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(observed["complete"].as_bool(), Some(false));
    assert_eq!(observed["vm_retirement_confirmed"].as_bool(), Some(false));
    assert_eq!(
        aterm_json::to_vec(&observed["guest_result"]).unwrap(),
        aterm_json::to_vec(&report).unwrap()
    );

    let scratch = aterm_tempfile::tempdir().unwrap();
    let finished = std::cell::Cell::new(false);
    assert_eq!(
        finish_qualification(scratch.path(), Ok((0, report.clone())), || {
            assert!(!scratch.path().join("result.json").exists());
            finished.set(true);
            Ok(())
        })
        .unwrap(),
        0
    );
    assert!(finished.get());
    let saved: Value =
        aterm_json::from_slice(&std::fs::read(scratch.path().join("result.json")).unwrap())
            .unwrap();
    assert_eq!(
        aterm_json::to_vec(&saved).unwrap(),
        aterm_json::to_vec(&report).unwrap()
    );
}

#[test]
fn failed_qualification_survives_unavailable_retirement_without_publication() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let failed = finish_qualification(
        scratch.path(),
        Ok((1, json!({"complete":true,"qualification":false}))),
        || Err(mac::not_run("checked retirement was unavailable")),
    );
    assert_eq!(failed.unwrap_err().code, 1);
    assert!(!scratch.path().join("result.json").exists());
}
