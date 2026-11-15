// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn identity() -> Identity {
    let mut identity = aterm_uds::process::read_identity(std::process::id() as i32).unwrap();
    identity.idversion = Some(1);
    identity
}
fn child_identity() -> Identity {
    let mut child = identity();
    child.pid += 1;
    child
}
fn retired_sample(at: u64) -> Sample {
    Sample {
        at,
        total: 120,
        originals: vec![child_identity().token()],
        retired: vec![child_identity().token()],
        roles: BTreeMap::from([("controller".into(), vec![member(Some((100, 120))).unwrap()])]),
    }
}

fn member(memory: Option<(u64, u64)>) -> Result<Value, String> {
    let identity = identity();
    checked_member(
        identity,
        (Some(identity), Some(identity)),
        "/fixture".into(),
        memory,
        "digest".into(),
        None,
        None,
    )
}
#[test]
fn actual_member_validation_refuses_unknown_zero_and_changed_identity() {
    assert!(member(Some((100, 120))).is_ok());
    for memory in [None, Some((0, 100)), Some((100, 0))] {
        assert!(member(memory).is_err());
    }
    let identity = identity();
    for bad in [
        None,
        Some(Identity {
            pid: identity.pid + 1,
            ..identity
        }),
        Some(Identity {
            idversion: Some(2),
            ..identity
        }),
    ] {
        assert!(
            checked_member(
                identity,
                (bad, Some(identity)),
                "/fixture".into(),
                Some((100, 120)),
                "digest".into(),
                None,
                None
            )
            .is_err()
        );
        assert!(
            checked_member(
                identity,
                (Some(identity), bad),
                "/fixture".into(),
                Some((100, 120)),
                "digest".into(),
                None,
                None
            )
            .is_err()
        );
    }
    assert!(
        checked_member(
            identity,
            (Some(identity), Some(identity)),
            "relative".into(),
            Some((100, 120)),
            "digest".into(),
            None,
            None
        )
        .is_err()
    );
}
#[test]
fn recorder_preserves_raw_but_refuses_over_budget_stale_or_failed_completion() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let sample = |at| Sample {
        at,
        total: 120,
        retired: Vec::new(),
        originals: Vec::new(),
        roles: BTreeMap::from([("controller".into(), vec![member(Some((100, 120))).unwrap()])]),
    };
    let path = dir.path().join("raw.jsonl");
    let mut writer = Writer::new(
        path.clone(),
        "key".into(),
        "generation".into(),
        120,
        sample(1),
    )
    .unwrap();
    assert!(writer.observe("fixture", sample(1)).is_err());
    let mut excessive = sample(2);
    excessive.total = 121;
    assert!(writer.observe("fixture", excessive).is_err());
    assert!(
        writer
            .finish(Some("actual capture refusal".into()), None)
            .is_err()
    );
    let raw = std::fs::read_to_string(path).unwrap();
    assert!(raw.contains("actual capture refusal"));
    let writer = Writer::new(
        dir.path().join("complete.jsonl"),
        "key".into(),
        "generation".into(),
        120,
        sample(3),
    )
    .unwrap();
    let report = CaptureState::default()
        .complete(writer, &identity().token(), Ok(retired_sample(4)))
        .unwrap();
    assert_eq!(report.witnesses["controller"].len(), 2);
    assert_eq!(
        aterm_update_core::sha256_file(&report.raw_path).unwrap(),
        report.raw_digest
    );
}

#[test]
fn production_capture_state_keeps_cleanup_budget_read_and_persistence_failures_terminal() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let sample = |at, total| Sample {
        at,
        total,
        retired: Vec::new(),
        originals: Vec::new(),
        roles: BTreeMap::from([("controller".into(), vec![member(Some((100, 120))).unwrap()])]),
    };
    for failure in 0..3 {
        let path = dir.path().join(format!("sticky-{failure}.jsonl"));
        let mut writer = Writer::new(
            path.clone(),
            "key".into(),
            "generation".into(),
            120,
            sample(1, 120),
        )
        .unwrap();
        if failure == 2 {
            // An actual read-only file produces the same write/flush failure as the timer.
            writer.out = BufWriter::new(std::fs::File::open(&path).unwrap());
        }
        let mut state = CaptureState::default();
        let actual = match failure {
            0 => Ok(sample(2, 121)),
            1 => Err("live cleanup member footprint unavailable".into()),
            _ => Ok(sample(2, 120)),
        };
        assert!(state.observe(&mut writer, "cleanup", actual).is_err());
        // Quiet later readings and a successful external stop cannot reset the refusal.
        assert!(
            state
                .observe(&mut writer, "cleanup", Ok(sample(3, 120)))
                .is_err()
        );
        assert!(
            state
                .complete(writer, &identity().token(), Ok(retired_sample(4)))
                .is_err()
        );
        if failure != 2 {
            let raw = std::fs::read_to_string(path).unwrap();
            assert!(raw.contains("refused"));
            assert!(raw.contains("recorder_stop"));
        }
    }
}

#[test]
fn cleanup_only_retires_positively_gone_originals_and_records_them_without_zeroes() {
    let original = child_identity();
    assert_eq!(
        presence(original, Some(original), true, true).unwrap(),
        Presence::Live
    );
    assert_eq!(
        presence(original, None, false, true).unwrap(),
        Presence::Retired
    );
    assert!(presence(original, None, true, true).is_err());
    assert!(presence(original, None, false, false).is_err());
    let changed = Identity {
        idversion: Some(2),
        ..original
    };
    assert!(presence(original, Some(changed), true, true).is_err());
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("retired.jsonl");
    let sample = |at, retired| Sample {
        at,
        total: 120,
        retired,
        originals: vec![original.token()],
        roles: BTreeMap::from([("controller".into(), vec![member(Some((100, 120))).unwrap()])]),
    };
    let mut writer = Writer::new(
        path.clone(),
        "key".into(),
        "generation".into(),
        120,
        sample(1, Vec::new()),
    )
    .unwrap();
    let mut state = CaptureState::default();
    state
        .observe(
            &mut writer,
            "cleanup",
            Ok(sample(2, vec![original.token()])),
        )
        .unwrap();
    let report = state
        .complete(writer, &identity().token(), Ok(retired_sample(4)))
        .unwrap();
    assert_eq!(report.raw_path, path);
    let text = std::fs::read_to_string(path).unwrap();
    let row: Value = aterm_json::from_str(text.lines().nth(2).unwrap()).unwrap();
    assert_eq!(
        row["retired_original_identities"],
        json!([original.token()])
    );
    assert_eq!(
        row["components"]["controller"][0]["footprint_bytes"],
        json!(100)
    );
}

#[test]
fn production_final_completion_refuses_a_known_survivor_after_valid_draining() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("survivor.jsonl");
    let survivor = |at| {
        let mut sample = retired_sample(at);
        sample.retired.clear();
        let mut child = member(Some((100, 120))).unwrap();
        child
            .as_object_mut()
            .unwrap()
            .insert("identity".into(), json!(child_identity().token()));
        sample.roles.insert("producers".into(), vec![child]);
        sample.total = 240;
        sample
    };
    let mut writer = Writer::new(
        path.clone(),
        "key".into(),
        "generation".into(),
        4096,
        survivor(1),
    )
    .unwrap();
    let mut state = CaptureState::default();
    state
        .observe(&mut writer, "cleanup", Ok(survivor(2)))
        .unwrap();
    let refused = state
        .complete(writer, &identity().token(), Ok(survivor(3)))
        .err()
        .unwrap();
    assert!(refused.contains("surviving or unretired"), "{refused}");
    let raw = std::fs::read_to_string(path).unwrap();
    let footer: Value = aterm_json::from_str(raw.lines().last().unwrap()).unwrap();
    assert_eq!(footer["cleanup_complete"], json!(false));
    assert!(footer["error"].as_str().unwrap().contains("unretired"));
    // The exact same production completion path accepts only the all-retired trace.
    let writer = Writer::new(
        dir.path().join("retired-complete.jsonl"),
        "key".into(),
        "generation".into(),
        4096,
        survivor(1),
    )
    .unwrap();
    CaptureState::default()
        .complete(writer, &identity().token(), Ok(retired_sample(2)))
        .unwrap();
}

#[test]
fn recorder_keeps_unordered_raw_counters_and_selects_the_complete_envelope() {
    let dir = aterm_tempfile::tempdir().unwrap();
    for (index, (current, lifetime)) in [(101, 100), (1_000_000, 1), (100, 900)]
        .into_iter()
        .enumerate()
    {
        let observed = member(Some((current, lifetime))).unwrap();
        assert_eq!(observed["footprint_bytes"], json!(current));
        assert_eq!(observed["lifetime_peak_footprint_bytes"], json!(lifetime));
        let envelope = current.max(lifetime);
        assert_eq!(peak_sum(std::slice::from_ref(&observed)).unwrap(), envelope);
        assert_eq!(
            peak_sum(&[observed.clone(), observed.clone()]).unwrap(),
            (current * 2).max(lifetime)
        );
        let path = dir.path().join(format!("unordered-{index}.jsonl"));
        let sample = |at| Sample {
            at,
            total: envelope,
            originals: vec![child_identity().token()],
            retired: vec![child_identity().token()],
            roles: BTreeMap::from([("controller".into(), vec![observed.clone()])]),
        };
        let mut writer = Writer::new(
            path.clone(),
            "key".into(),
            "generation".into(),
            envelope,
            sample(1),
        )
        .unwrap();
        writer.observe("fixture", sample(2)).unwrap();
        let report = CaptureState::default()
            .complete(writer, &identity().token(), Ok(sample(3)))
            .unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        let header: Value = aterm_json::from_str(raw.lines().next().unwrap()).unwrap();
        assert_eq!(header["footprint_semantics"], json!(FOOTPRINT_SEMANTICS));
        for line in raw.lines().skip(1) {
            let row: Value = aterm_json::from_str(line).unwrap();
            if row["event"] == json!("sweep") {
                assert_eq!(row["components"]["controller"][0], observed);
            }
        }
        assert_eq!(report.witnesses["controller"][0]["members"][0], observed);
        for key in [
            "footprint_bytes",
            "lifetime_peak_footprint_bytes",
            "footprint_semantics",
        ] {
            let mut bad = observed.clone();
            bad.as_object_mut().unwrap().insert(key.into(), Value::Null);
            assert!(peak_sum(&[bad]).is_err());
        }
    }
    let huge = member(Some((u64::MAX, 1))).unwrap();
    assert!(peak_sum(&[huge.clone(), huge]).is_err());
}
