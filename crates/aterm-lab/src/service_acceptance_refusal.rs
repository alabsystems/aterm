// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A lab caller may reject acceptance after genuine native proof. Production owns every
//! service operation, transaction journal, rollback and nonce result. This does not inject
//! a process crash, unhealthy service, transport failure or production fault option.

use super::{Live, error};
use crate::{Fail, Res};
use aterm_agent::service_activation::{NativeBackend, signed_outcome, verified_receipt};
use aterm_json::{Value, json};
use aterm_secrets::{
    Result,
    client::Paths,
    service::Job,
    service_acceptance::{self, ActivationCustody, Environment},
    service_repair::{
        self as transaction, Backend, HealthRequirement, Observed, Outcome, Request, Unit,
    },
};
use std::path::{Path, PathBuf};

const MAX_RECORD: usize = 1024 * 1024;
const KIND: &str = "lab_caller_rejection_after_native_proof";
const USAGE: &str = "service-acceptance reject-verified ID SIGNED_REQUEST CTL_EXECUTABLE (lab caller rejection, not candidate crash or native RPC)";

fn digest(value: &Value) -> Result<String> {
    let bytes = aterm_json::to_vec(value).map_err(error)?;
    Ok(hex(&aterm_digest::Sha256::digest(bytes)))
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
#[derive(Debug)]
struct VerifiedCandidateRefusal<'a> {
    evidence_sha256: &'a str,
}
impl std::fmt::Display for VerifiedCandidateRefusal<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{KIND}: evidence-sha256={}",
            self.evidence_sha256
        )
    }
}
fn refusal_detail(evidence: &str) -> String {
    VerifiedCandidateRefusal {
        evidence_sha256: evidence,
    }
    .to_string()
}

trait ProvedBackend: Backend {
    fn native_evidence(&self, request: &Request) -> Result<Value>;
}
impl ProvedBackend for NativeBackend {
    fn native_evidence(&self, request: &Request) -> Result<Value> {
        let proof = self
            .proof_for(request.job, &request.unit)
            .filter(|proof| proof.health() == &request.health)
            .ok_or_else(|| error("successful exact native candidate proof is absent"))?;
        Ok(proof.value())
    }
}

trait Custody {
    fn consumed(&self) -> bool;
    fn check(&mut self, observed: &Observed) -> Result<()>;
    fn record(&mut self, proof: Value) -> Result<String>;
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Unarmed,
    Verified,
    Installed,
    Consumed,
}
struct RejectVerified<B, C> {
    inner: B,
    custody: C,
    request: Request,
    phase: Phase,
}
impl<B: ProvedBackend, C: Custody> RejectVerified<B, C> {
    fn new(inner: B, custody: C, request: Request) -> Self {
        let phase = if custody.consumed() {
            Phase::Consumed
        } else {
            Phase::Unarmed
        };
        Self {
            inner,
            custody,
            request,
            phase,
        }
    }
    fn check(&mut self, job: Job) -> Result<()> {
        let observed = self.inner.observe(job)?;
        self.custody.check(&observed)
    }
}
impl<B: ProvedBackend, C: Custody> Backend for RejectVerified<B, C> {
    fn authorize(&mut self, request: &Request) -> Result<()> {
        if request != &self.request {
            return Err(error(
                "lab activation differs from its bound signed request",
            ));
        }
        self.inner.authorize(request)?;
        self.check(request.job)
    }
    fn observe(&mut self, job: Job) -> Result<Observed> {
        self.inner.observe(job)
    }
    fn verify_candidate(&mut self, request: &Request) -> Result<()> {
        if request != &self.request {
            return Err(error("lab candidate differs from its bound signed request"));
        }
        self.inner.verify_candidate(request)?;
        if self.phase != Phase::Consumed {
            self.phase = Phase::Verified;
        }
        Ok(())
    }
    fn install(&mut self, job: Job, unit: &Unit) -> Result<()> {
        self.check(job)?;
        // Recovery runs before candidate verification. It must remain entirely unarmed,
        // even if an older journal restores this same program as its predecessor.
        let arm =
            self.phase == Phase::Verified && job == self.request.job && unit == &self.request.unit;
        self.inner.install(job, unit)?;
        if self.phase != Phase::Consumed {
            self.phase = if arm {
                Phase::Installed
            } else {
                Phase::Unarmed
            };
        }
        Ok(())
    }
    fn prove(&mut self, job: Job, unit: &Unit, health: &HealthRequirement) -> Result<()> {
        self.inner.prove(job, unit, health)?;
        if self.phase != Phase::Installed || job != self.request.job || unit != &self.request.unit {
            return Ok(());
        }
        // Capture the immutable successful proof before observation clears the backend's
        // last-proof slot. The custody check then refuses any newly observed policy drift.
        let proof = self.inner.native_evidence(&self.request)?;
        self.check(job)?;
        let evidence = self.custody.record(proof)?;
        self.phase = Phase::Consumed;
        Err(error(VerifiedCandidateRefusal {
            evidence_sha256: &evidence,
        }))
    }
}

struct OwnedCustody {
    paths: Paths,
    id: String,
    request: Request,
    captured: ActivationCustody,
    guard: Live,
    path: PathBuf,
    record: Option<Value>,
    signed_request_digest: String,
    floor: u64,
}
impl OwnedCustody {
    fn load(&mut self) -> Result<()> {
        let Some(bytes) = aterm_secrets::fsx::read_private(&self.path, MAX_RECORD)? else {
            return Ok(());
        };
        let value: Value = aterm_json::from_slice(bytes.as_slice()).map_err(error)?;
        let injection = &value["injection"];
        if value["schema"].as_u64() != Some(1)
            || injection["kind"].as_str() != Some(KIND)
            || injection["phase"].as_str() != Some("candidate_verified_after_install")
            || injection["custody_sha256"].as_str() != Some(self.captured.custody_sha256())
            || injection["signed_request_sha256"].as_str() != Some(&self.signed_request_digest)
            || injection["custody_id"].as_str() != Some(&self.id)
            || injection["request_sha256"].as_str()
                != Some(digest(&transaction::request_value(&self.request))?.as_str())
            || injection["nonce"].as_str() != Some(&self.request.nonce)
            || injection["baseline_build_floor"].as_u64() != Some(self.floor)
            || value["injection_sha256"].as_str() != Some(digest(injection)?.as_str())
        {
            return Err(error(
                "lab refusal record does not match owned custody and signed request",
            ));
        }
        self.record = Some(value);
        Ok(())
    }
    fn write(&self, value: &Value, first: bool) -> Result<()> {
        self.write_with_sync(value, first, aterm_durable::durable_dir)
    }
    fn write_with_sync(
        &self,
        value: &Value,
        first: bool,
        sync_dir: impl FnOnce(&Path) -> std::io::Result<()>,
    ) -> Result<()> {
        let bytes = aterm_json::to_vec(value).map_err(error)?;
        if bytes.len() > MAX_RECORD {
            return Err(error(
                "lab refusal evidence exceeds its recovery read limit",
            ));
        }
        let parent = self
            .path
            .parent()
            .ok_or_else(|| error("missing evidence parent"))?;
        aterm_secrets::fsx::ensure_private_dir(parent)?;
        aterm_durable::durable_dir(
            parent
                .parent()
                .ok_or_else(|| error("missing custody parent"))?,
        )?;
        if first {
            if !aterm_secrets::fsx::write_new(&self.path, &bytes, 0o600)? {
                return Err(error(
                    "lab refusal evidence already exists; injection not repeated",
                ));
            }
            // write_new's directory flush is best effort. Check this barrier before
            // delivering the typed rejection, retaining the placed file on failure.
            sync_dir(parent)?;
        } else {
            aterm_secrets::fsx::write_atomic(&self.path, &bytes, 0o600)?;
        }
        Ok(())
    }
    fn record_with_sync(
        &mut self,
        proof: Value,
        sync_dir: impl FnOnce(&Path) -> std::io::Result<()>,
    ) -> Result<String> {
        if self.record.is_some() {
            return Err(error("lab refusal has already been recorded"));
        }
        let injection = json!({"kind":KIND,"phase":"candidate_verified_after_install","custody_id":self.id,
            "custody_sha256":self.captured.custody_sha256(),"signed_request_sha256":self.signed_request_digest,
            "request_sha256":digest(&transaction::request_value(&self.request))?,"nonce":self.request.nonce,
            "baseline_build_floor":self.floor,"build_floor_source":"qualified_service_baseline",
            "native_proof":proof});
        let evidence = digest(&injection)?;
        let record = json!({"schema":1,"injection":injection,"injection_sha256":evidence,
            "delivery":"unconfirmed"});
        self.write_with_sync(&record, true, sync_dir)?;
        self.record = Some(record);
        Ok(evidence)
    }
    fn finish(&mut self, outcome: &Outcome, signed: Value) -> Result<Value> {
        let mut confirmed = false;
        if let Some(mut record) = self.record.clone() {
            let evidence = record["injection_sha256"]
                .as_str()
                .ok_or_else(|| error("missing injection digest"))?;
            confirmed = matches!(
                outcome.verdict,
                transaction::Verdict::RolledBack | transaction::Verdict::RollbackFailed
            ) && outcome.detail.starts_with(&refusal_detail(evidence));
            let object = record
                .as_object_mut()
                .ok_or_else(|| error("invalid refusal record"))?;
            object.insert(
                "delivery".into(),
                json!(if confirmed {
                    "confirmed_by_signed_outcome"
                } else {
                    "unconfirmed"
                }),
            );
            object.insert("signed_outcome".into(), signed.clone());
            self.write(&record, false)?;
            self.record = Some(record);
        }
        Ok(
            json!({"schema":1,"scope":"lab_caller_rejection_after_native_proof",
            "acceptance_passed":confirmed && outcome.verdict == transaction::Verdict::RolledBack,
            "rejection_delivery_confirmed":confirmed,"candidate_crash_tested":false,
            "native_rpc_tested":false,"signed_outcome":signed,"evidence_path":self.path}),
        )
    }
}
impl Custody for OwnedCustody {
    fn consumed(&self) -> bool {
        self.record.is_some()
    }
    fn check(&mut self, observed: &Observed) -> Result<()> {
        self.guard.guard()?;
        self.captured
            .check_current(&self.paths, &self.id, &self.request, observed)
    }
    fn record(&mut self, proof: Value) -> Result<String> {
        self.record_with_sync(proof, aterm_durable::durable_dir)
    }
}

fn run(id: &str, request_path: &Path, ctl: &Path) -> Result<Value> {
    let paths = Paths::live()?;
    let bytes =
        aterm_secrets::fsx::read_private(request_path, transaction::MAX_REQUEST_BYTES + 4096)?
            .ok_or_else(|| error("signed request is missing"))?;
    let signed: Value = aterm_json::from_slice(bytes.as_slice()).map_err(error)?;
    let request = transaction::parse_request(&signed["request"])?;
    let captured = service_acceptance::validate_activation(&paths, id, &request)?;
    let receipt = verified_receipt(&paths, &request.desired_image)?;
    let floor = receipt.build;
    let baseline = Path::new(&captured.baseline().program)
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .ok_or_else(|| error("qualified baseline is not an app bundle"))?;
    if floor == 0
        || captured.baseline().executable_sha256 != receipt.service.executable_sha256
        || aterm_update_core::service_image::image_digest(baseline).map_err(error)?
            != receipt.service.image_sha256
    {
        return Err(error(
            "baseline build policy is not bound to the same qualified signed image",
        ));
    }
    let native = NativeBackend::new(paths.clone(), Some(signed.clone()), floor);
    let signed_request_digest = digest(&signed)?;
    let mut custody = OwnedCustody {
        paths: paths.clone(),
        id: id.into(),
        request: request.clone(),
        captured,
        guard: Live {
            paths: paths.clone(),
            ctl: ctl.into(),
            baseline: None,
            qualified: None,
        },
        path: paths
            .dir
            .join("service-acceptance")
            .join("refusals")
            .join(format!("{signed_request_digest}.json")),
        record: None,
        signed_request_digest,
        floor,
    };
    custody.load()?;
    let mut adapter = RejectVerified::new(native, custody, request.clone());
    let outcome = transaction::activate(&paths.home, &request, &mut adapter)?;
    adapter.custody.finish(&outcome, signed_outcome(&outcome)?)
}

pub(super) fn cmd(args: &[String]) -> Res {
    let [id, request, ctl] = args else {
        return Err(Fail::usage(USAGE));
    };
    let report =
        run(id, Path::new(request), Path::new(ctl)).map_err(|e| Fail::die(e.to_string()))?;
    println!(
        "{}",
        aterm_json::to_string(&report).map_err(|e| Fail::die(e.to_string()))?
    );
    Ok(if report["acceptance_passed"].as_bool() == Some(true) {
        0
    } else {
        1
    })
}

#[cfg(test)]
mod tests {
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
            &journal.join(format!("{}.json",request.job.label())),
            &aterm_json::to_vec(&json!({"schema":1,"request":transaction::request_value(&request),"phase":"prepared"})).unwrap(),
            0o600,
        ).unwrap();
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
}
