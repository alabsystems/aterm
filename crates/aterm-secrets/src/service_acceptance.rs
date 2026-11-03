// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Dev-only custody of a physical server acceptance run's original service unit.
//! Signed activation remains the production protocol; this seam only installs the
//! qualified temporary baseline and restores the exact original after the exercise.

use crate::{
    Error, Result,
    client::Paths,
    service::{self, Job},
    service_repair::{self, HealthRequirement, Observed, Request, Unit},
};
use aterm_json::{Value, json};
use std::path::{Path, PathBuf};

const MAX_CUSTODY_BYTES: usize = 1024 * 1024;

/// Actual service operations and the caller's current hold/lease authorization.
pub trait Environment {
    /// Refuse current holds, leases or disabled sharing before every mutation.
    fn guard(&mut self) -> Result<()>;
    /// Read the actual local server unit, Off record and pin.
    fn observe(&mut self) -> Result<Observed>;
    /// Install an exact unit without changing Off or pin records.
    fn install(&mut self, unit: &Unit) -> Result<()>;
    /// Prove its kernel image and authenticated served vault/frontier.
    fn prove(&mut self, unit: &Unit, health: &HealthRequirement) -> Result<()>;
}

fn failure(message: impl Into<String>) -> Error {
    Error::new(message.into())
}
fn record_path(paths: &Paths, id: &str) -> Result<PathBuf> {
    if !(16..=128).contains(&id.len())
        || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(failure(
            "acceptance id must be 16..128 ASCII letters, digits or hyphens",
        ));
    }
    Ok(paths
        .dir
        .join("service-acceptance")
        .join(format!("{id}.json")))
}
fn write(path: &Path, value: &Value) -> Result<()> {
    let bytes = aterm_json::to_vec(value).map_err(|e| failure(e.to_string()))?;
    if bytes.len() > MAX_CUSTODY_BYTES {
        return Err(failure(
            "acceptance custody record exceeds its recovery read limit",
        ));
    }
    // Recovery parses each embedded request independently. Validate both before a
    // write, even when the complete record still fits its enclosing read limit.
    snapshot(value)?;
    let parent = path
        .parent()
        .ok_or_else(|| failure("acceptance record has no parent"))?;
    crate::fsx::ensure_private_dir(parent)?;
    aterm_durable::durable_dir(
        parent
            .parent()
            .ok_or_else(|| failure("acceptance directory has no parent"))?,
    )?;
    crate::fsx::write_atomic(path, &bytes, 0o600)
}
fn same_unit(a: &Unit, b: &Unit) -> bool {
    a.program == b.program
        && a.args == b.args
        && a.definition == b.definition
        && a.executable_sha256 == b.executable_sha256
}
fn check_observed(
    now: &Observed,
    original: &Observed,
    baseline: &Unit,
    candidate: &Unit,
) -> Result<()> {
    if now.off != original.off || now.pin != original.pin {
        return Err(failure("acceptance restore held: Off or pin changed"));
    }
    let old = original
        .unit
        .as_ref()
        .ok_or_else(|| failure("acceptance original service is absent"))?;
    if now.unit.as_ref().is_none_or(|unit| {
        ![old, baseline, candidate]
            .iter()
            .any(|allowed| same_unit(unit, allowed))
    }) {
        return Err(failure(
            "acceptance restore held: service unit changed outside this run",
        ));
    }
    Ok(())
}
fn snapshot(value: &Value) -> Result<(Request, Request)> {
    let Some(object) = value.as_object() else {
        return Err(failure("unsupported acceptance custody record"));
    };
    if object.len() != 4
        || ["schema", "phase", "baseline", "candidate"]
            .iter()
            .any(|field| !object.contains_key(*field))
        || value["schema"].as_u64() != Some(1)
        || !matches!(
            value["phase"].as_str(),
            Some("prepared" | "baseline" | "restored")
        )
    {
        return Err(failure("unsupported acceptance custody record"));
    }
    let baseline = service_repair::parse_request(&value["baseline"])?;
    let candidate = service_repair::parse_request(&value["candidate"])?;
    if baseline.job != Job::Server
        || candidate.job != Job::Server
        || baseline.precondition != candidate.precondition
        || baseline.precondition.off
        || baseline.precondition.pin.is_some()
        || baseline.precondition.unit.is_none()
        || baseline.health != candidate.health
        || baseline.nonce != candidate.nonce
        || baseline.node != candidate.node
        || baseline.requester != candidate.requester
        || baseline.current_image != candidate.current_image
        || baseline.desired_image != candidate.desired_image
        || baseline.required_protocols != candidate.required_protocols
        || baseline.unit.args != candidate.unit.args
        || same_unit(&baseline.unit, &candidate.unit)
    {
        return Err(failure("acceptance custody records disagree"));
    }
    Ok((baseline, candidate))
}

/// Immutable custody of the qualified images and original service for one activation.
/// This dev seam grants no fleet, signature or image authority. The production backend
/// independently checks those before the transaction can change a service.
pub struct ActivationCustody {
    record: PathBuf,
    sha256: String,
    baseline: Request,
    candidate: Request,
}

impl ActivationCustody {
    /// The exact candidate service definition captured by the acceptance run.
    pub fn candidate(&self) -> &Unit {
        &self.candidate.unit
    }

    /// The physical baseline service definition installed by the acceptance run.
    pub fn baseline(&self) -> &Unit {
        &self.baseline.unit
    }

    /// The original service and human decisions retained for restoration.
    pub fn original(&self) -> &Observed {
        &self.baseline.precondition
    }

    /// SHA-256 of the exact private custody record bytes, including their phase.
    pub fn custody_sha256(&self) -> &str {
        &self.sha256
    }

    fn check_request(&self, request: &Request) -> Result<()> {
        if request.job != Job::Server
            || request.unit != self.candidate.unit
            || request.desired_image != self.candidate.desired_image
        {
            return Err(failure(
                "activation does not name the exact candidate held in acceptance custody",
            ));
        }
        if request
            .precondition
            .unit
            .as_ref()
            .is_none_or(|unit| !same_unit(unit, &self.baseline.unit))
            || request.precondition.off != self.original().off
            || request.precondition.pin != self.original().pin
        {
            return Err(failure(
                "activation precondition differs from the acceptance baseline or original Off/pin",
            ));
        }
        if request.health != self.baseline.health
            || request.required_protocols != self.baseline.required_protocols
        {
            return Err(failure(
                "activation health or protocols differ from the acceptance snapshot",
            ));
        }
        Ok(())
    }

    /// Recheck custody and the physical service while the caller holds the job decision
    /// lock. Only the original, baseline or candidate unit with unchanged Off/pin is allowed.
    /// # Errors
    /// A different record path, unreadable or changed bytes, request drift, policy drift,
    /// or a service unit outside this acceptance run. This method takes no lock or action.
    pub fn check_current(
        &self,
        paths: &Paths,
        id: &str,
        request: &Request,
        observed: &Observed,
    ) -> Result<()> {
        let path = record_path(paths, id)?;
        if path != self.record {
            return Err(failure("activation custody record path changed"));
        }
        let bytes = crate::fsx::read_private(&path, MAX_CUSTODY_BYTES)?
            .ok_or_else(|| failure("acceptance record is absent"))?;
        if custody_digest(bytes.as_slice()) != self.sha256 {
            return Err(failure("activation custody record bytes changed"));
        }
        self.check_request(request)?;
        check_observed(observed, self.original(), self.baseline(), self.candidate())
    }
}

fn custody_digest(bytes: &[u8]) -> String {
    aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(bytes))
}

/// Validate an activation against an existing private acceptance custody record.
/// The activation must retain the captured health/protocol expectations, physically
/// start at the baseline and name the exact candidate. Node, requester, signature and
/// current-image authority remain the production backend's independent responsibility.
/// # Errors
/// Invalid id/schema, missing or unreadable custody, a phase other than baseline, or
/// candidate, baseline, policy, health or protocol drift. No locks or mutations occur.
pub fn validate_activation(
    paths: &Paths,
    id: &str,
    request: &Request,
) -> Result<ActivationCustody> {
    let record = record_path(paths, id)?;
    let bytes = crate::fsx::read_private(&record, MAX_CUSTODY_BYTES)?
        .ok_or_else(|| failure("acceptance record is absent"))?;
    let value: Value =
        aterm_json::from_slice(bytes.as_slice()).map_err(|error| failure(error.to_string()))?;
    let (baseline, candidate) = snapshot(&value)?;
    if baseline.nonce != id {
        return Err(failure("acceptance custody id mismatch"));
    }
    if value["phase"].as_str() != Some("baseline") {
        return Err(failure("activation custody is not in the baseline phase"));
    }
    let custody = ActivationCustody {
        record,
        sha256: custody_digest(bytes.as_slice()),
        baseline,
        candidate,
    };
    custody.check_request(request)?;
    Ok(custody)
}

/// Capture the current unpinned, enabled server and install a qualified temporary baseline.
/// The lab verifies both supplied image paths against the same signed release before calling.
/// The durable record precedes every service mutation; an interruption is resumed by `restore`.
/// # Errors
/// A hold, changed decision, missing service, invalid image, failed journal, install or proof.
#[expect(
    clippy::too_many_arguments,
    reason = "custody binds both image paths, signed identities, health and the guarded environment"
)]
pub fn begin(
    paths: &Paths,
    id: &str,
    baseline_program: &Path,
    candidate_program: &Path,
    image: &str,
    executable_sha256: &str,
    health: HealthRequirement,
    environment: &mut impl Environment,
) -> Result<Value> {
    let path = record_path(paths, id)?;
    let _decision = service::JobDecision::take(&paths.home, Job::Server)?;
    environment.guard()?;
    if crate::fsx::kind(&path)? != crate::fsx::Kind::Missing {
        return Err(failure(
            "acceptance id already has a custody record; restore it first",
        ));
    }
    let original = environment.observe()?;
    if original.off || original.pin.is_some() {
        return Err(failure("acceptance never overrides Off or an explicit pin"));
    }
    let old = original
        .unit
        .as_ref()
        .ok_or_else(|| failure("acceptance requires an existing server"))?;
    environment.prove(old, &health)?;
    let baseline = service::repair_unit(&paths.home, Job::Server, old, baseline_program, image)?;
    let candidate = service::repair_unit(&paths.home, Job::Server, old, candidate_program, image)?;
    if baseline.executable_sha256 != executable_sha256
        || candidate.executable_sha256 != executable_sha256
        || same_unit(old, &baseline)
        || same_unit(&baseline, &candidate)
    {
        return Err(failure(
            "acceptance images do not match the distinct qualified baseline and candidate",
        ));
    }
    let request = Request {
        node: "local-acceptance".into(),
        requester: "local-acceptance".into(),
        nonce: id.into(),
        job: Job::Server,
        current_image: old.image.clone(),
        desired_image: image.into(),
        precondition: original.clone(),
        unit: baseline,
        health,
        required_protocols: vec![1],
    };
    let mut candidate_request = request.clone();
    candidate_request.unit = candidate;
    let mut value = json!({"schema":1,"phase":"prepared","baseline":service_repair::request_value(&request),
        "candidate":service_repair::request_value(&candidate_request)});
    write(&path, &value)?;
    environment.guard()?;
    if environment.observe()? != original {
        return Err(failure(
            "acceptance original service changed before baseline installation",
        ));
    }
    if let Err(error) = environment
        .install(&request.unit)
        .and_then(|()| environment.prove(&request.unit, &request.health))
    {
        let restored = restore_locked(paths, id, environment);
        return Err(failure(match restored {
            Ok(_) => format!("{error}; original server restored and proven"),
            Err(restore) => {
                format!("{error}; original server restoration remains recorded: {restore}")
            }
        }));
    }
    value
        .as_object_mut()
        .expect("record is object")
        .insert("phase".into(), json!("baseline"));
    write(&path, &value)?;
    Ok(
        json!({"phase":"baseline","record":path,"original":service_repair::request_value(&request)["precondition"],
        "candidate_program":candidate_request.unit.program,"image":image}),
    )
}

/// Restore only a unit this custody record replaced, with unchanged Off/pin and no current hold.
/// # Errors
/// Missing/invalid record, human policy drift, external unit changes, failed restore or proof.
pub fn restore(paths: &Paths, id: &str, environment: &mut impl Environment) -> Result<Value> {
    let _decision = service::JobDecision::take(&paths.home, Job::Server)?;
    restore_locked(paths, id, environment)
}

fn restore_locked(paths: &Paths, id: &str, environment: &mut impl Environment) -> Result<Value> {
    let path = record_path(paths, id)?;
    environment.guard()?;
    let bytes = crate::fsx::read_private(&path, MAX_CUSTODY_BYTES)?
        .ok_or_else(|| failure("acceptance record is absent"))?;
    let mut value: Value =
        aterm_json::from_slice(bytes.as_slice()).map_err(|e| failure(e.to_string()))?;
    let (baseline, candidate) = snapshot(&value)?;
    if baseline.nonce != id {
        return Err(failure("acceptance custody id mismatch"));
    }
    let now = environment.observe()?;
    check_observed(
        &now,
        &baseline.precondition,
        &baseline.unit,
        &candidate.unit,
    )?;
    let old = baseline
        .precondition
        .unit
        .as_ref()
        .ok_or_else(|| failure("missing original unit"))?;
    environment.guard()?;
    if now.unit.as_ref().is_none_or(|unit| !same_unit(unit, old)) {
        environment.install(old)?;
    }
    environment.prove(old, &baseline.health)?;
    if environment.observe()? != baseline.precondition {
        return Err(failure("restored server differs from original snapshot"));
    }
    value
        .as_object_mut()
        .expect("record is object")
        .insert("phase".into(), json!("restored"));
    write(&path, &value)?;
    Ok(
        json!({"phase":"restored","record":path,"kernel_image_proven":true,"signed_server_frontier_proven":true,
            "legacy_listener_nonce_proven":false}),
    )
}

#[cfg(test)]
mod tests {
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
}
