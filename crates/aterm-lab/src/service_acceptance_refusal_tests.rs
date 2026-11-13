// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

struct Fake {
    observed: Observed,
    installs: Vec<Unit>,
    proof: Option<Unit>,
    reject_install: bool,
    reject_proof: bool,
}
impl Backend for Fake {
    fn authorize(&mut self, _: &Request) -> Result<()> {
        Ok(())
    }
    fn observe(&mut self, _: Job) -> Result<Observed> {
        Ok(self.observed.clone())
    }
    fn verify_candidate(&mut self, _: &Request) -> Result<()> {
        Ok(())
    }
    fn install(&mut self, _: Job, unit: &Unit) -> Result<()> {
        self.proof = None;
        if self.reject_install {
            return Err(error("real install failed"));
        }
        self.installs.push(unit.clone());
        self.observed.unit = Some(unit.clone());
        Ok(())
    }
    fn prove(&mut self, _: Job, unit: &Unit, _: &HealthRequirement) -> Result<()> {
        self.proof = None;
        if self.reject_proof {
            return Err(error("real native proof failed"));
        }
        self.proof = Some(unit.clone());
        Ok(())
    }
}
impl ProvedBackend for Fake {
    fn native_evidence(&self, request: &Request) -> Result<Value> {
        if self.proof.as_ref() != Some(&request.unit) {
            return Err(error("no exact native proof"));
        }
        Ok(json!({"pid":7,"unit":transaction::request_value(request)["unit"]}))
    }
}
#[derive(Default)]
struct MemoryCustody {
    records: Vec<Value>,
    held: bool,
    write_failed: bool,
}
impl Custody for MemoryCustody {
    fn consumed(&self) -> bool {
        !self.records.is_empty()
    }
    fn check(&mut self, _: &Observed) -> Result<()> {
        if self.held {
            Err(error("held"))
        } else {
            Ok(())
        }
    }
    fn record(&mut self, proof: Value) -> Result<String> {
        if self.write_failed {
            return Err(error("evidence could not be persisted"));
        }
        let sha = digest(&proof)?;
        self.records.push(proof);
        Ok(sha)
    }
}
fn request() -> Request {
    let old = Unit {
        program: "/qualified/baseline.app/Contents/MacOS/aterm".into(),
        args: vec!["secrets".into(), "serve".into()],
        definition: "baseline definition".into(),
        image: "same-image".into(),
        executable_sha256: "same-executable".into(),
    };
    let mut new = old.clone();
    new.program = "/store/candidate.app/Contents/MacOS/aterm".into();
    new.definition = "candidate definition".into();
    Request {
        node: "target".into(),
        requester: "requester".into(),
        nonce: "lab-refusal-nonce-001".into(),
        job: Job::Server,
        current_image: old.image.clone(),
        desired_image: new.image.clone(),
        precondition: Observed {
            unit: Some(old),
            off: false,
            pin: None,
        },
        unit: new,
        health: HealthRequirement {
            vault: "team".into(),
            signer: "signer".into(),
            records: vec!["record".into()],
            frontier: 1,
        },
        required_protocols: vec![1],
    }
}
fn adapter(request: &Request) -> RejectVerified<Fake, MemoryCustody> {
    RejectVerified::new(
        Fake {
            observed: request.precondition.clone(),
            installs: vec![],
            proof: None,
            reject_install: false,
            reject_proof: false,
        },
        MemoryCustody::default(),
        request.clone(),
    )
}

#[test]
fn real_transaction_rolls_back_after_exact_candidate_proof_and_replays_nonce() {
    let home = aterm_tempfile::tempdir().unwrap();
    let request = request();
    let mut adapter = adapter(&request);
    let outcome = transaction::activate(home.path(), &request, &mut adapter).unwrap();
    assert_eq!(outcome.verdict, transaction::Verdict::RolledBack);
    assert!(outcome.detail.starts_with(KIND));
    assert_eq!(
        adapter.inner.installs,
        [
            request.unit.clone(),
            request.precondition.unit.clone().unwrap()
        ]
    );
    assert_eq!(adapter.custody.records.len(), 1);
    assert_eq!(adapter.inner.observed, request.precondition);
    assert_eq!(
        transaction::activate(home.path(), &request, &mut adapter).unwrap(),
        outcome
    );
    assert_eq!(adapter.inner.installs.len(), 2);
    assert_eq!(adapter.custody.records.len(), 1);
    let mut conflicting = request.clone();
    conflicting.health.frontier += 1;
    adapter.request = conflicting.clone();
    assert!(transaction::activate(home.path(), &conflicting, &mut adapter).is_err());
    assert_eq!(adapter.inner.installs.len(), 2);
}

#[test]
fn baseline_recovery_and_same_hash_other_paths_never_inject() {
    let request = request();
    let old = request.precondition.unit.as_ref().unwrap();
    let mut adapter = adapter(&request);
    // Even a recovery which installs the exact future candidate remains unarmed.
    adapter.install(request.job, &request.unit).unwrap();
    adapter
        .prove(request.job, &request.unit, &request.health)
        .unwrap();
    adapter.prove(request.job, old, &request.health).unwrap();
    assert!(adapter.custody.records.is_empty());
    adapter.verify_candidate(&request).unwrap();
    adapter.install(request.job, old).unwrap();
    adapter.prove(request.job, old, &request.health).unwrap();
    assert!(adapter.custody.records.is_empty());
    assert_eq!(old.image, request.unit.image);
    assert_eq!(old.executable_sha256, request.unit.executable_sha256);
}

#[test]
fn failed_native_operations_and_failed_evidence_never_claim_injection() {
    let request = request();
    for fail_install in [true, false] {
        let mut adapter = adapter(&request);
        adapter.verify_candidate(&request).unwrap();
        if fail_install {
            adapter.inner.reject_install = true;
            assert!(adapter.install(request.job, &request.unit).is_err());
        } else {
            adapter.install(request.job, &request.unit).unwrap();
            adapter.inner.reject_proof = true;
            assert!(
                adapter
                    .prove(request.job, &request.unit, &request.health)
                    .is_err()
            );
        }
        assert!(adapter.custody.records.is_empty());
    }
    let mut adapter = adapter(&request);
    adapter.verify_candidate(&request).unwrap();
    adapter.install(request.job, &request.unit).unwrap();
    adapter.custody.write_failed = true;
    let failure = adapter
        .prove(request.job, &request.unit, &request.health)
        .unwrap_err();
    assert!(!failure.to_string().contains(KIND));
    assert!(adapter.custody.records.is_empty());
}

#[test]
fn held_custody_prevents_mutation_and_consumed_evidence_prevents_reinjection() {
    let request = request();
    let mut held = adapter(&request);
    held.custody.held = true;
    assert!(held.authorize(&request).is_err());
    assert!(held.install(request.job, &request.unit).is_err());
    assert!(held.inner.installs.is_empty());
    let mut consumed = adapter(&request);
    consumed.custody.records.push(json!({"previous":"proof"}));
    consumed.phase = Phase::Consumed;
    consumed.verify_candidate(&request).unwrap();
    consumed.install(request.job, &request.unit).unwrap();
    consumed
        .prove(request.job, &request.unit, &request.health)
        .unwrap();
    assert_eq!(consumed.custody.records.len(), 1);
}

#[test]
fn interrupted_prepared_transaction_recovers_before_the_adapter_can_arm() {
    let home = aterm_tempfile::tempdir().unwrap();
    let request = request();
    let journal = home.path().join(".config/aterm/secrets/service-repair");
    aterm_secrets::fsx::ensure_private_dir(&journal).unwrap();
    aterm_secrets::fsx::write_atomic(
        &journal.join(format!("{}.json", request.job.label())),
        &aterm_json::to_vec(
            &json!({"schema":1,"request":transaction::request_value(&request),"phase":"prepared"}),
        )
        .unwrap(),
        0o600,
    )
    .unwrap();
    let mut adapter = adapter(&request);
    adapter.inner.observed.unit = Some(request.unit.clone());
    let outcome = transaction::activate(home.path(), &request, &mut adapter).unwrap();
    assert_eq!(outcome.verdict, transaction::Verdict::RolledBack);
    assert!(!outcome.detail.starts_with(KIND));
    assert_eq!(
        adapter.inner.installs,
        [request.precondition.unit.clone().unwrap()]
    );
    assert!(adapter.custody.records.is_empty());
}

fn owned(paths: &Paths, request: &Request) -> OwnedCustody {
    let id = "owned-lab-custody-001";
    let mut baseline = request.clone();
    baseline.nonce = id.into();
    baseline.unit = request.precondition.unit.clone().unwrap();
    let mut original = baseline.unit.clone();
    original.program = "/original.app/Contents/MacOS/aterm".into();
    original.definition = "original unit".into();
    original.image = "original".into();
    baseline.current_image = original.image.clone();
    baseline.precondition.unit = Some(original);
    let mut candidate = baseline.clone();
    candidate.unit = request.unit.clone();
    let parent = paths.dir.join("service-acceptance");
    aterm_secrets::fsx::ensure_private_dir(&parent).unwrap();
    let custody_path = parent.join(format!("{id}.json"));
    if !custody_path.exists() {
        aterm_secrets::fsx::write_atomic(&custody_path,
            &aterm_json::to_vec(&json!({"schema":1,"phase":"baseline",
                "baseline":transaction::request_value(&baseline),"candidate":transaction::request_value(&candidate)})).unwrap(),0o600).unwrap();
    }
    let captured = service_acceptance::validate_activation(paths, id, request).unwrap();
    OwnedCustody {
        paths: paths.clone(),
        id: id.into(),
        request: request.clone(),
        captured,
        guard: Live {
            paths: paths.clone(),
            ctl: "/never-executed".into(),
            baseline: None,
            qualified: None,
        },
        path: parent.join("refusals").join("test-evidence.json"),
        record: None,
        signed_request_digest: "bound-envelope-digest".into(),
        floor: 1,
    }
}

#[test]
fn durable_proof_does_not_confirm_delivery_until_the_production_outcome_does() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    let request = request();
    let mut original = owned(&paths, &request);
    let evidence = original.record(json!({"native":"fixture proof"})).unwrap();
    let mut resumed = owned(&paths, &request);
    resumed.load().unwrap();
    assert!(resumed.consumed());
    assert_eq!(
        resumed.record.as_ref().unwrap()["delivery"].as_str(),
        Some("unconfirmed")
    );
    // A killed caller can recover successfully without ever having delivered the
    // injected refusal. Restoration alone must not confirm that delivery.
    let mut outcome = Outcome {
        request: request.clone(),
        verdict: transaction::Verdict::RolledBack,
        detail: "recovered interrupted activation".into(),
    };
    let report = resumed
        .finish(&outcome, json!({"test":"native outcome fixture"}))
        .unwrap();
    assert_eq!(report["acceptance_passed"].as_bool(), Some(false));
    assert_eq!(
        report["rejection_delivery_confirmed"].as_bool(),
        Some(false)
    );
    outcome.detail = refusal_detail(&evidence);
    let report = resumed
        .finish(&outcome, json!({"test":"delivered outcome fixture"}))
        .unwrap();
    assert_eq!(report["acceptance_passed"].as_bool(), Some(true));
    let mut reread = owned(&paths, &request);
    reread.load().unwrap();
    assert!(reread.consumed());
    assert_eq!(
        reread.record.as_ref().unwrap()["delivery"].as_str(),
        Some("confirmed_by_signed_outcome")
    );
    outcome.verdict = transaction::Verdict::RollbackFailed;
    assert_eq!(
        reread
            .finish(&outcome, json!({"test":"failed restore fixture"}))
            .unwrap()["acceptance_passed"]
            .as_bool(),
        Some(false)
    );
}

#[test]
fn oversized_or_misbound_evidence_cannot_be_persisted_or_reused() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    let request = request();
    let mut custody = owned(&paths, &request);
    assert!(
        custody
            .record(json!({"oversized":"x".repeat(MAX_RECORD)}))
            .is_err()
    );
    assert!(custody.record.is_none());
    assert!(!custody.path.exists());
    custody.record(json!({"native":"fixture proof"})).unwrap();
    let mut other = owned(&paths, &request);
    other.signed_request_digest = "different-envelope".into();
    assert!(other.load().is_err());
}

#[test]
fn failed_first_directory_barrier_retains_evidence_without_delivering_or_reinjecting() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    let request = request();
    let mut custody = owned(&paths, &request);
    let failure = custody
        .record_with_sync(json!({"native":"fixture proof"}), |_| {
            Err(std::io::Error::other("first directory barrier failed"))
        })
        .unwrap_err();
    assert!(!failure.to_string().starts_with(KIND));
    assert!(
        custody.path.is_file(),
        "placed evidence must never be removed on flush failure"
    );
    assert!(
        custody.record.is_none(),
        "no delivered rejection can be claimed"
    );
    let mut resumed = owned(&paths, &request);
    resumed.load().unwrap();
    assert!(resumed.consumed());
    assert!(resumed.record(json!({"native":"second proof"})).is_err());
    let outcome = Outcome {
        request,
        verdict: transaction::Verdict::RolledBack,
        detail: failure.to_string(),
    };
    let report = resumed
        .finish(&outcome, json!({"test":"flush refusal outcome fixture"}))
        .unwrap();
    assert_eq!(report["acceptance_passed"].as_bool(), Some(false));
    assert_eq!(
        report["rejection_delivery_confirmed"].as_bool(),
        Some(false)
    );
}
