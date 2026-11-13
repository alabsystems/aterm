// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
struct Fake {
    observed: Observed,
    held: bool,
    installs: usize,
    record: Option<PathBuf>,
    reject_program: Option<String>,
}
impl Environment for Fake {
    fn guard(&mut self) -> Result<()> {
        if self.held {
            Err(failure("held"))
        } else {
            Ok(())
        }
    }
    fn observe(&mut self) -> Result<Observed> {
        Ok(self.observed.clone())
    }
    fn install(&mut self, unit: &Unit) -> Result<()> {
        if let Some(path) = &self.record {
            assert!(
                path.is_file(),
                "journal must precede the first service mutation"
            );
        }
        self.installs += 1;
        self.observed.unit = Some(unit.clone());
        Ok(())
    }
    fn prove(&mut self, unit: &Unit, _: &HealthRequirement) -> Result<()> {
        if self.reject_program.as_deref() == Some(unit.program.as_str()) {
            Err(failure("injected unhealthy baseline"))
        } else {
            Ok(())
        }
    }
}
fn unit(program: &str) -> Unit {
    Unit {
        program: program.into(),
        args: vec!["secrets".into(), "serve".into()],
        definition: format!("unit {program}"),
        image: program.into(),
        executable_sha256: program.into(),
    }
}

fn activation_fixture(paths: &Paths, id: &str) -> (Value, Request) {
    let original = Observed {
        unit: Some(unit("/original/aterm")),
        off: false,
        pin: None,
    };
    let mut baseline = unit("/qualified/baseline/aterm");
    baseline.image = "qualified-image".into();
    baseline.executable_sha256 = "same-qualified-executable".into();
    let mut candidate = unit("/qualified/candidate/aterm");
    candidate.image = baseline.image.clone();
    candidate.executable_sha256 = baseline.executable_sha256.clone();
    let captured = Request {
        node: "local-acceptance".into(),
        requester: "local-acceptance".into(),
        nonce: id.into(),
        job: Job::Server,
        current_image: original.unit.as_ref().unwrap().image.clone(),
        desired_image: "qualified-image".into(),
        precondition: original,
        unit: baseline,
        health: HealthRequirement {
            vault: "captured-vault".into(),
            signer: "captured-signer".into(),
            records: vec!["captured-frontier".into()],
            frontier: 7,
        },
        required_protocols: vec![1],
    };
    let mut candidate_snapshot = captured.clone();
    candidate_snapshot.unit = candidate;
    let record = json!({"schema":1,"phase":"baseline",
        "baseline":service_repair::request_value(&captured),
        "candidate":service_repair::request_value(&candidate_snapshot)});
    write(&record_path(paths, id).unwrap(), &record).unwrap();
    let mut activation = candidate_snapshot;
    activation.node = "authenticated-target".into();
    activation.requester = "authenticated-controller".into();
    activation.nonce = "signed-activation-nonce".into();
    activation.current_image = captured.unit.image.clone();
    activation.precondition.unit = Some(captured.unit);
    (record, activation)
}

#[test]
fn activation_custody_binds_distinct_paths_despite_identical_executable_hashes() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(root.path());
    let id = "activation-custody-test";
    let (_, mut request) = activation_fixture(&paths, id);
    let custody = validate_activation(&paths, id, &request).unwrap();
    assert_eq!(custody.candidate(), &request.unit);
    assert_eq!(
        custody.baseline(),
        request.precondition.unit.as_ref().unwrap()
    );
    assert_ne!(custody.baseline().program, custody.candidate().program);
    assert_eq!(
        custody.baseline().executable_sha256,
        custody.candidate().executable_sha256
    );
    assert_eq!(
        custody.custody_sha256(),
        custody_digest(&std::fs::read(record_path(&paths, id).unwrap()).unwrap())
    );
    request.precondition.unit.as_mut().unwrap().image = "legacy-observation-label".into();
    validate_activation(&paths, id, &request).unwrap();
    for expected in [
        custody.original().unit.as_ref().unwrap(),
        custody.baseline(),
        custody.candidate(),
    ] {
        custody
            .check_current(
                &paths,
                id,
                &request,
                &Observed {
                    unit: Some(expected.clone()),
                    off: false,
                    pin: None,
                },
            )
            .unwrap();
    }
    let mut wrong = request.clone();
    wrong.unit = custody.baseline().clone();
    assert!(validate_activation(&paths, id, &wrong).is_err());
    wrong = request.clone();
    wrong.precondition.unit = Some(custody.candidate().clone());
    assert!(validate_activation(&paths, id, &wrong).is_err());
}

#[test]
fn activation_custody_refuses_request_drift() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(root.path());
    let id = "activation-drift-test";
    let (_, request) = activation_fixture(&paths, id);
    let custody = validate_activation(&paths, id, &request).unwrap();
    let observed = request.precondition.clone();
    for change in [
        "job",
        "program",
        "hash",
        "args",
        "definition",
        "image",
        "desired",
        "baseline-path",
        "baseline-hash",
        "baseline-args",
        "baseline-definition",
        "off",
        "pin",
        "vault",
        "signer",
        "records",
        "frontier",
        "protocol",
    ] {
        let mut changed = request.clone();
        match change {
            "job" => changed.job = Job::Subscriber,
            "program" => changed.unit.program.push_str("-elsewhere"),
            "hash" => changed.unit.executable_sha256.push_str("-changed"),
            "args" => changed.unit.args.push("--changed".into()),
            "definition" => changed.unit.definition.push_str(" changed"),
            "image" => changed.unit.image.push_str("-changed"),
            "desired" => changed.desired_image.push_str("-changed"),
            "baseline-path" => changed
                .precondition
                .unit
                .as_mut()
                .unwrap()
                .program
                .push_str("-elsewhere"),
            "baseline-hash" => changed
                .precondition
                .unit
                .as_mut()
                .unwrap()
                .executable_sha256
                .push_str("-changed"),
            "baseline-args" => changed
                .precondition
                .unit
                .as_mut()
                .unwrap()
                .args
                .push("--changed".into()),
            "baseline-definition" => changed
                .precondition
                .unit
                .as_mut()
                .unwrap()
                .definition
                .push_str(" changed"),
            "off" => changed.precondition.off = true,
            "pin" => changed.precondition.pin = Some("explicit pin".into()),
            "vault" => changed.health.vault.push_str("-changed"),
            "signer" => changed.health.signer.push_str("-changed"),
            "records" => changed.health.records.push("another-frontier".into()),
            "frontier" => changed.health.frontier += 1,
            "protocol" => changed.required_protocols.push(2),
            _ => unreachable!(),
        }
        assert!(
            validate_activation(&paths, id, &changed).is_err(),
            "{change}"
        );
        assert!(
            custody
                .check_current(&paths, id, &changed, &observed)
                .is_err(),
            "{change}"
        );
    }
}

#[test]
fn activation_custody_refuses_invalid_record_identity_schema_and_phase() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(root.path());
    let id = "activation-record-test";
    let (record, request) = activation_fixture(&paths, id);
    assert!(validate_activation(&paths, "../not-an-id", &request).is_err());
    let other = "another-record-id";
    write(&record_path(&paths, other).unwrap(), &record).unwrap();
    assert!(validate_activation(&paths, other, &request).is_err());
    for (field, value) in [
        ("schema", json!(2)),
        ("phase", json!("prepared")),
        ("phase", json!("restored")),
        ("extra", json!(true)),
    ] {
        let mut changed = record.clone();
        changed.as_object_mut().unwrap().insert(field.into(), value);
        crate::fsx::write_atomic(
            &record_path(&paths, id).unwrap(),
            &aterm_json::to_vec(&changed).unwrap(),
            0o600,
        )
        .unwrap();
        assert!(
            validate_activation(&paths, id, &request).is_err(),
            "{field}"
        );
    }
}

#[test]
fn activation_custody_rechecks_bytes_and_current_physical_decisions() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(root.path());
    let id = "activation-current-test";
    let (_, request) = activation_fixture(&paths, id);
    let custody = validate_activation(&paths, id, &request).unwrap();
    for changed in [
        Observed {
            off: true,
            ..request.precondition.clone()
        },
        Observed {
            pin: Some("new pin".into()),
            ..request.precondition.clone()
        },
        Observed {
            unit: Some(unit("unknown-unit")),
            ..request.precondition.clone()
        },
        Observed {
            unit: None,
            ..request.precondition.clone()
        },
    ] {
        assert!(
            custody
                .check_current(&paths, id, &request, &changed)
                .is_err()
        );
    }
    assert!(
        custody
            .check_current(&paths, "different-run-id", &request, &request.precondition)
            .is_err()
    );
    let path = record_path(&paths, id).unwrap();
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.push(b'\n');
    crate::fsx::write_atomic(&path, &bytes, 0o600).unwrap();
    validate_activation(&paths, id, &request).unwrap();
    assert!(
        custody
            .check_current(&paths, id, &request, &request.precondition)
            .unwrap_err()
            .to_string()
            .contains("record bytes changed")
    );
}

#[test]
fn restore_preserves_human_decisions_and_rejects_external_units() {
    let old = unit("old");
    let baseline = unit("baseline");
    let candidate = unit("candidate");
    let original = Observed {
        unit: Some(old.clone()),
        off: false,
        pin: None,
    };
    for changed in [
        Observed {
            off: true,
            ..original.clone()
        },
        Observed {
            pin: Some("pin".into()),
            ..original.clone()
        },
        Observed {
            unit: Some(unit("external")),
            ..original.clone()
        },
    ] {
        assert!(check_observed(&changed, &original, &baseline, &candidate).is_err());
    }
    for expected in [&old, &baseline, &candidate] {
        assert!(
            check_observed(
                &Observed {
                    unit: Some(expected.clone()),
                    ..original.clone()
                },
                &original,
                &baseline,
                &candidate
            )
            .is_ok()
        );
    }
}
#[test]
fn durable_restore_is_exact_idempotent_and_held_before_effects() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(root.path());
    let id = "acceptance-test-0001";
    let old = unit("old");
    let baseline = unit("baseline");
    let candidate = unit("candidate");
    let original = Observed {
        unit: Some(old.clone()),
        off: false,
        pin: None,
    };
    let request = Request {
        node: "local".into(),
        requester: "local".into(),
        nonce: id.into(),
        job: Job::Server,
        current_image: "old".into(),
        desired_image: "new".into(),
        precondition: original.clone(),
        unit: baseline,
        health: HealthRequirement {
            vault: "team".into(),
            signer: "key".into(),
            records: vec!["record".into()],
            frontier: 1,
        },
        required_protocols: vec![1],
    };
    let mut wanted = request.clone();
    wanted.unit = candidate.clone();
    let record = json!({"schema":1,"phase":"baseline","baseline":service_repair::request_value(&request),"candidate":service_repair::request_value(&wanted)});
    let path = record_path(&paths, id).unwrap();
    write(&path, &record).unwrap();
    let previous = std::fs::read(&path).unwrap();
    let mut too_large = record.clone();
    too_large
        .as_object_mut()
        .unwrap()
        .insert("padding".into(), json!("x".repeat(MAX_CUSTODY_BYTES)));
    assert!(write(&path, &too_large).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), previous);
    let mut unreadable_request = record.clone();
    unreadable_request
        .as_object_mut()
        .unwrap()
        .get_mut("baseline")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .get_mut("unit")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert(
            "definition".into(),
            json!("x".repeat(service_repair::MAX_REQUEST_BYTES)),
        );
    assert!(aterm_json::to_vec(&unreadable_request).unwrap().len() < MAX_CUSTODY_BYTES);
    assert!(write(&path, &unreadable_request).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), previous);
    let mut fake = Fake {
        observed: Observed {
            unit: Some(candidate),
            ..original.clone()
        },
        held: true,
        installs: 0,
        record: None,
        reject_program: None,
    };
    assert!(restore(&paths, id, &mut fake).is_err());
    assert_eq!(fake.installs, 0);
    fake.held = false;
    restore(&paths, id, &mut fake).unwrap();
    assert_eq!(fake.observed, original);
    assert_eq!(fake.installs, 1);
    restore(&paths, id, &mut fake).unwrap();
    assert_eq!(fake.installs, 1);

    let mut extra_field = record.clone();
    extra_field
        .as_object_mut()
        .unwrap()
        .insert("unexpected".into(), json!(true));
    assert!(snapshot(&extra_field).is_err());

    let mut pinned_precondition = record.clone();
    for side in ["baseline", "candidate"] {
        pinned_precondition
            .as_object_mut()
            .unwrap()
            .get_mut(side)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .get_mut("precondition")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("pin".into(), json!("pinned"));
    }
    assert!(snapshot(&pinned_precondition).is_err());

    let mut disagreed_image = record.clone();
    disagreed_image
        .as_object_mut()
        .unwrap()
        .get_mut("candidate")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert("desired_image".into(), json!("other-image"));
    assert!(snapshot(&disagreed_image).is_err());

    let mut duplicate_unit = record.clone();
    duplicate_unit
        .as_object_mut()
        .unwrap()
        .get_mut("candidate")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert("unit".into(), record["baseline"]["unit"].clone());
    assert!(snapshot(&duplicate_unit).is_err());
}

#[test]
fn baseline_mutation_has_durable_custody_and_failed_health_restores_original() {
    for fail in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(root.path());
        let old = root.path().join("old");
        let baseline = root.path().join("baseline");
        let candidate = root.path().join("candidate");
        std::fs::write(&old, b"old image").unwrap();
        std::fs::write(&baseline, b"qualified image").unwrap();
        std::fs::write(&candidate, b"qualified image").unwrap();
        let args = vec!["secrets".to_owned(), "serve".to_owned()];
        let definition = if cfg!(target_os = "macos") {
            service::plist_text(
                Job::Server.label(),
                &old,
                &args,
                &service::log_path(root.path(), Job::Server),
            )
        } else {
            service::systemd_unit(Job::Server.description(), &old, &args)
        };
        let old_hash = service::repair_image_hash(&old).unwrap();
        let original = Observed {
            unit: Some(Unit {
                program: old.to_str().unwrap().into(),
                args,
                definition,
                image: old_hash.clone(),
                executable_sha256: old_hash,
            }),
            off: false,
            pin: None,
        };
        let id = "baseline-custody-test";
        let mut env = Fake {
            observed: original.clone(),
            held: false,
            installs: 0,
            record: Some(record_path(&paths, id).unwrap()),
            reject_program: fail.then(|| baseline.to_str().unwrap().into()),
        };
        let oversized = begin(
            &paths,
            id,
            &baseline,
            &candidate,
            &"a".repeat(64),
            &service::repair_image_hash(&baseline).unwrap(),
            HealthRequirement {
                vault: "team".into(),
                signer: "key".into(),
                records: vec!["x".repeat(service_repair::MAX_REQUEST_BYTES)],
                frontier: 1,
            },
            &mut env,
        );
        assert!(oversized.is_err());
        assert_eq!(env.installs, 0);
        assert!(!record_path(&paths, id).unwrap().exists());
        let result = begin(
            &paths,
            id,
            &baseline,
            &candidate,
            &"a".repeat(64),
            &service::repair_image_hash(&baseline).unwrap(),
            HealthRequirement {
                vault: "team".into(),
                signer: "key".into(),
                records: vec!["record".into()],
                frontier: 1,
            },
            &mut env,
        );
        if fail {
            assert!(result.is_err());
            assert_eq!(env.observed, original);
            assert_eq!(env.installs, 2);
        } else {
            result.unwrap();
            assert_eq!(env.installs, 1);
            restore(&paths, id, &mut env).unwrap();
            assert_eq!(env.observed, original);
            assert_eq!(env.installs, 2);
        }
    }
}
