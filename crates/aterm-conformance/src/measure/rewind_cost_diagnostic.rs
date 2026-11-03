// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit nonacceptance recording observations using the canonical cost take.

use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::Path;
use std::time::Instant;

use aterm_json::{Value, json};
use aterm_uds::process::read_identity;

use super::super::Rig;
use super::super::instance::{Band, Budget, CouldNotRun, could_not_run, field};
use super::{BUDGET_SECS, Take, cpu_ns, recording_drops};

const BYTES: usize = 64 << 20;
const STAGES: &[&str] = &[
    "ingest",
    "copy",
    "reader_lock",
    "parse",
    "record",
    "witness",
    "apply",
    "mint",
    "commit",
];

fn sha256(path: &Path) -> Result<String, CouldNotRun> {
    use std::fmt::Write as _;
    let mut file =
        crate::measure::open_regular_file(path).map_err(|e| CouldNotRun::Refused(e.to_string()))?;
    let mut digest = aterm_digest::Sha256::new();
    let mut bytes = [0; 8192];
    loop {
        let n = file
            .read(&mut bytes)
            .map_err(|e| CouldNotRun::Refused(e.to_string()))?;
        if n == 0 {
            break;
        }
        digest.update(&bytes[..n]);
    }
    let mut out = String::with_capacity(64);
    for byte in digest.finalize() {
        let _ = write!(out, "{byte:02x}");
    }
    Ok(out)
}

fn record(out: &mut File, value: &Value) -> Result<(), CouldNotRun> {
    writeln!(out, "{value}")
        .and_then(|()| out.flush())
        .map_err(|e| CouldNotRun::Refused(e.to_string()))
}

fn trace_fields(status: &str, on: bool) -> Result<(), CouldNotRun> {
    let any = status
        .split_whitespace()
        .any(|word| word.starts_with("recording_cost_"));
    if !on {
        return if any {
            could_not_run("cost fields appeared with the diagnostic opt-in OFF")
        } else {
            Ok(())
        };
    }
    if field(status, "recording_cost_scope").is_none() {
        return could_not_run("subject did not expose requested recording cost diagnostics");
    }
    for stage in STAGES {
        for suffix in ["calls", "work", "wall_ns", "cpu_ns", "cpu_unknown_scopes"] {
            let key = format!("recording_cost_{stage}_{suffix}");
            if !field(status, &key)
                .is_some_and(|value| value == "-" || value.parse::<u64>().is_ok())
            {
                return could_not_run(format!("subject omitted or malformed {key}"));
            }
        }
    }
    Ok(())
}

fn role(pid: u32) -> Value {
    let (value, errno) = Band::observed_role(pid);
    json!({"darwin_role": value, "errno": errno, "policy": "read_only_role_observation"})
}

#[derive(Clone, Copy)]
enum Plan {
    Tax,
    Reference,
}

impl Plan {
    fn arms(self) -> &'static [(bool, bool)] {
        match self {
            Self::Tax => &[
                (false, false),
                (false, true),
                (false, true),
                (false, false),
                (true, false),
                (true, true),
                (true, true),
                (true, false),
            ],
            Self::Reference => &[(false, false), (true, false), (true, false), (false, false)],
        }
    }

    fn reference(self) -> bool {
        matches!(self, Self::Reference)
    }
}

fn reference_roles(subject: &Value, driver: &Value) -> bool {
    subject["darwin_role"].as_i64() == Some(1)
        && driver["darwin_role"].as_i64() == Some(0)
        && subject.get("errno").is_some_and(Value::is_null)
        && driver.get("errno").is_some_and(Value::is_null)
}

/// Collect eight fresh 64MiB takes: recording OFF then ON, each with probe OFF/ON/ON/OFF.
/// No ratio, threshold, or passing acceptance verdict is produced. Scratch worlds are kept.
/// # Errors
/// I/O, launch, missing/malformed evidence, changed process/image identity, or recorded loss.
pub fn collect(rig: &Rig, directory: &Path) -> Result<(), CouldNotRun> {
    collect_plan(rig, directory, Plan::Tax)
}

/// Observe one four-arm reference block, recording OFF/ON/ON/OFF with probes OFF.
/// Actual subject focal1 and inherited driver role0 must match the prior tax campaign.
/// No acceptance verdict is produced and scratch worlds are kept.
/// # Errors
/// As [`collect`], or the actual roles are unknown or differ from the required context.
pub fn collect_reference(rig: &Rig, directory: &Path) -> Result<(), CouldNotRun> {
    collect_plan(rig, directory, Plan::Reference)
}

fn collect_plan(rig: &Rig, directory: &Path, plan: Plan) -> Result<(), CouldNotRun> {
    let arms = plan.arms();
    let metadata = directory
        .metadata()
        .map_err(|e| CouldNotRun::Refused(e.to_string()))?;
    if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 {
        return could_not_run("diagnostic output must be an existing owner-private directory");
    }
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(directory.join("diagnostic.jsonl"))
        .map_err(|e| CouldNotRun::Refused(e.to_string()))?;
    let subject_hash = sha256(&rig.aterm)?;
    let producer_hash = sha256(&rig.fake_tui)?;
    let driver = std::env::current_exe().map_err(|e| CouldNotRun::Refused(e.to_string()))?;
    record(
        &mut out,
        &json!({
            "event": "plan", "schema": 2, "diagnostic_only": true, "bytes_requested": BYTES,
            "plan": if plan.reference() { "reference_four_arms" } else { "instrumentation_tax_eight_arms" },
            "arms": arms.iter().map(|&(recording, probe)| json!({"recording":recording,"probe":probe})).collect::<Vec<_>>(),
            "planned_arms": arms.len(),
            "role_requirement": if plan.reference() { "canonical_subject_take; no_additional_driver_override; require_subject_focal1_driver0_or_refuse" } else { "canonical_subject_take_or_refusal; no_additional_driver_override" },
            "subject": rig.aterm, "subject_sha256": subject_hash,
            "producer": rig.fake_tui, "producer_sha256": producer_hash,
            "driver": driver, "driver_sha256": sha256(&driver)?,
            "driver_debug_assertions": cfg!(debug_assertions), "driver_role": role(0),
            "source_profile_provenance": "see separately retained frozen build manifests; current checkout is not compiled source proof",
            "launch": "canonical Take/World/Instance lifeline, role take/refusal; only explicit diagnostic child env differs",
            "scope": "CPU brackets include setup and polling; stage scopes inclusive, polling nonatomic, boundary probe overhead; not drain-only CPU",
            "timing_acceptance": "not_evaluated; original five-ABBA/970 row unchanged",
        }),
    )?;
    let budget = Budget::from_now(BUDGET_SECS);
    let mut arm = 0;
    let result = (|| {
        for &(recording, probe) in arms {
            let (take, enabled) = Take::start_with_diagnostic(
                rig,
                &format!("temporal_recording = {recording}"),
                budget,
                Some(probe),
            )?;
            let identity = read_identity(take.inst.pid().cast_signed())
                .ok_or_else(|| CouldNotRun::Refused("subject identity unavailable".into()))?;
            let before = take.inst.ctl.ok(&["temporal", "status"])?;
            let listing = take.inst.ctl.call(&["ls"]);
            let subject_role = role(take.inst.pid());
            let driver_role = role(0);
            record(
                &mut out,
                &json!({
                    "event": "arm-started", "arm": arm, "recording": recording, "probe": probe,
                    "subject_identity": identity.token(), "subject_band_take": take.inst.band.to_string(),
                    "subject_role": subject_role, "driver_role": driver_role,
                    "scratch": take._scratch.path(), "status_before": before,
                    "pane_list": listing.stdout, "pane_list_exit": listing.code,
                    "grid_before": take.inst.ctl.ok(&["status"])?,
                }),
            )?;
            if plan.reference()
                && (take.inst.band != Band::Focal || !reference_roles(&subject_role, &driver_role))
            {
                return could_not_run(
                    "reference requires observed subject focal1 and driver role0; canonical subject take retained, no additional driver override attempted",
                );
            }
            if enabled != recording
                || !listing.ok()
                || listing
                    .stdout
                    .lines()
                    .filter(|line| !line.is_empty())
                    .count()
                    != 1
            {
                return could_not_run(
                    "recording state or one-pane context did not match the diagnostic arm",
                );
            }
            trace_fields(&before, probe)?;
            let report = take.home.join("drain.us");
            let cpu_before = cpu_ns(take.inst.pid());
            let started = Instant::now();
            take.start_drain(rig, None, BYTES, &report)?;
            let drain_us = take.drained_us(&report)?;
            let receipt_wall_us = started.elapsed().as_micros();
            let cpu_at_receipt = cpu_ns(take.inst.pid());
            let receipt = take.inst.ctl.ok(&["temporal", "status"])?;
            record(
                &mut out,
                &json!({"event": "arm-receipt", "diagnostic_only": true, "arm": arm,
                        "recording": recording, "probe": probe, "producer_drain_us": drain_us,
                        "receipt_wall_us": receipt_wall_us, "cpu_before_ns": cpu_before,
                        "cpu_at_receipt_ns": cpu_at_receipt, "status_at_receipt": receipt}),
            )?;
            let completed = take.completed_status(recording)?;
            let complete_wall_us = started.elapsed().as_micros();
            let cpu_at_completion = cpu_ns(take.inst.pid());
            let same_identity = read_identity(take.inst.pid().cast_signed()) == Some(identity);
            let subject_role_completed = role(take.inst.pid());
            let driver_role_completed = role(0);
            record(
                &mut out,
                &json!({
                    "event": "arm-finished", "diagnostic_only": true, "arm": arm,
                    "recording": recording, "probe": probe, "producer_drain_us": drain_us,
                    "receipt_wall_us": receipt_wall_us, "complete_wall_us": complete_wall_us,
                    "cpu_before_ns": cpu_before, "cpu_at_receipt_ns": cpu_at_receipt,
                    "cpu_at_completion_ns": cpu_at_completion, "identity_unchanged": same_identity,
                    "status_at_receipt": receipt, "status_completed": completed,
                    "subject_role_completed": subject_role_completed, "driver_role_completed": driver_role_completed,
                }),
            )?;
            if !same_identity {
                return could_not_run("subject identity changed during arm");
            }
            if plan.reference() && !reference_roles(&subject_role_completed, &driver_role_completed)
            {
                return could_not_run("reference roles changed or became unknown during arm");
            }
            trace_fields(&completed, probe)?;
            for key in [
                "dropped_input",
                "dropped_events",
                "dropped_replies",
                "diverged",
            ] {
                if field(&completed, key).and_then(|value| value.parse::<u64>().ok()) != Some(0) {
                    return could_not_run(format!("diagnostic refused missing or nonzero {key}"));
                }
            }
            if recording {
                let _ = recording_drops(&completed)?;
            }
            arm += 1;
            eprintln!(
                "recording diagnostic: collected arm {arm}/{}; not an acceptance verdict",
                arms.len()
            );
        }
        if sha256(&rig.aterm)? != subject_hash || sha256(&rig.fake_tui)? != producer_hash {
            return could_not_run("subject or producer bytes changed during collection");
        }
        Ok(())
    })();
    record(
        &mut out,
        &json!({"event": "collection-ended", "diagnostic_only": true,
        "completed_arms": arm, "error": result.as_ref().err().map(ToString::to_string),
        "timing_acceptance": "not_evaluated"}),
    )?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_is_one_probe_free_abba_and_unknown_roles_refuse() {
        assert_eq!(
            Plan::Reference.arms(),
            &[(false, false), (true, false), (true, false), (false, false)]
        );
        let focal = json!({"darwin_role":1,"errno":null});
        let inherited = json!({"darwin_role":0,"errno":null});
        assert!(reference_roles(&focal, &inherited));
        assert!(!reference_roles(&json!({"darwin_role":1}), &inherited));
        assert!(!reference_roles(&inherited, &focal));
        assert!(!reference_roles(
            &focal,
            &json!({"darwin_role":null,"errno":1})
        ));
        assert!(!reference_roles(
            &json!({"darwin_role":null,"errno":null}),
            &inherited
        ));
    }

    #[test]
    fn opt_in_readback_is_required_and_never_accepted_in_the_off_arm() {
        assert!(trace_fields("OK enabled=true", false).is_ok());
        assert!(trace_fields("OK enabled=true", true).is_err());
        assert!(trace_fields("OK recording_cost_scope=partial", true).is_err());
        assert!(trace_fields("OK recording_cost_scope=anything", false).is_err());
        let mut complete = String::from(
            "OK recording_cost_scope=recorder_lifetime_completed,inclusive,polling_nonatomic",
        );
        for stage in STAGES {
            for suffix in ["calls", "work", "wall_ns", "cpu_ns", "cpu_unknown_scopes"] {
                complete.push_str(&format!(" recording_cost_{stage}_{suffix}=-"));
            }
        }
        assert!(
            trace_fields(&complete, true).is_ok(),
            "unknown counters remain observable unknowns"
        );
        assert!(
            trace_fields(
                &complete.replace(
                    "recording_cost_parse_cpu_ns=-",
                    "recording_cost_parse_cpu_ns=broken"
                ),
                true
            )
            .is_err()
        );
    }
}
