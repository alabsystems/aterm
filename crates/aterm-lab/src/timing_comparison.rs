// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One ordinary reservation per unchanged ABBA quad. The reservation is not cloneable;
//! an arm exclusively borrows it until recorded cleanup. No caller can submit a peer's
//! readiness sample or complete an arm with a diagnostic value. A failed/dropped arm
//! poisons the quad. Dropping the quad releases custody, never renews ordinary expiry.

use std::path::PathBuf;

use crate::instance::{Instance, Launch, World};
use crate::title_drift::{Build, Raw};

/// Versioned, independently sampled counters from the same process footprint ledger.
/// Apple XNU reads lifetime maximum before the later current value, so the pair
/// has no cross-field ordering guarantee (published xnu-12377.1.9):
/// <https://github.com/apple-oss-distributions/xnu/blob/f6217f891ac0bb64f3d375211650a4c1ff8ca1ea/bsd/kern/kern_resource.c#L3374>.
/// This does not identify the running kernel's exact update interleaving.
pub(super) const FOOTPRINT_SEMANTICS: &str = "proc_pid_rusage_v4_nonatomic_same_ledger_v1";

pub(super) struct FootprintObservation {
    pub(super) current: u64,
    pub(super) lifetime_max: u64,
}
impl FootprintObservation {
    pub(super) fn new(current: u64, lifetime_max: u64) -> Result<Self, String> {
        if current == 0 || lifetime_max == 0 {
            return Err(format!(
                "nonpositive footprint counters: current={current} lifetime_max={lifetime_max}"
            ));
        }
        Ok(Self {
            current,
            lifetime_max,
        })
    }
    pub(super) fn read(member: &aterm_json::Value) -> Result<Self, String> {
        footprint_semantics(member)?;
        let current = member
            .get("footprint_bytes")
            .and_then(aterm_json::Value::as_u64)
            .ok_or("unknown current footprint")?;
        let lifetime_max = member
            .get("lifetime_peak_footprint_bytes")
            .and_then(aterm_json::Value::as_u64)
            .ok_or("unknown kernel lifetime footprint")?;
        Self::new(current, lifetime_max)
    }
    pub(super) fn envelope(&self) -> u64 {
        self.current.max(self.lifetime_max)
    }
}
fn footprint_semantics(value: &aterm_json::Value) -> Result<(), String> {
    if value
        .get("footprint_semantics")
        .and_then(aterm_json::Value::as_str)
        != Some(FOOTPRINT_SEMANTICS)
    {
        return Err("unknown or missing footprint counter semantics".into());
    }
    Ok(())
}

/// Frozen passive experiment data, shared by memory preparation and quad admission.
/// It holds no resource authority and cannot be relabeled with another case or build.
pub(super) struct Experiment {
    builds: [Build; 2],
    case: String,
    pub(super) key: String,
    controller_digest: String,
    controller_path: PathBuf,
    quads: usize,
}
impl Experiment {
    pub(super) fn freeze(
        builds: &[Build; 2],
        case: String,
        work: String,
        quads: usize,
    ) -> Result<Self, String> {
        for build in builds {
            check_file(build)?;
        }
        let controller_path = crate::paths::paths().exe.clone();
        let controller_digest = aterm_update_core::sha256_file(&controller_path)?;
        let key = crate::measure::sha256(&format!(
            "footprint_semantics={FOOTPRINT_SEMANTICS} case={case} work={work} quads={quads} builds={builds:?} controller_path={controller_path:?} controller_sha256={controller_digest}"
        ));
        Ok(Self {
            builds: builds.clone(),
            case,
            key,
            controller_digest,
            controller_path,
            quads,
        })
    }
    pub(super) fn controller(&self) -> (&std::path::Path, &str) {
        (&self.controller_path, &self.controller_digest)
    }
    pub(super) fn expected_arms(&self) -> usize {
        self.quads * 4
    }
    pub(super) fn validate(&self) -> Result<(), String> {
        if aterm_update_core::sha256_file(&self.controller_path)? != self.controller_digest {
            return Err("frozen controller changed".into());
        }
        for build in &self.builds {
            check_file(build)?;
        }
        Ok(())
    }
}

/// Frozen, reviewable memory accounting. Component bounds include the controller,
/// app/keeper, all producers and setup/cleanup overlap; sequential arms use their maximum,
/// not the sum of four peaks. Evidence is supplied by the experiment preparer, never a
/// functional-admission override. There is deliberately no default numeric budget.
pub(super) struct Memory {
    bytes: u64,
    binding: String,
    pub(super) record: String,
    pub(super) digest: String,
    pub(super) evidence: Vec<(String, String)>,
    files: Vec<(PathBuf, String)>,
}

impl Memory {
    pub(super) fn read(path: PathBuf, experiment: &Experiment) -> Result<Self, String> {
        let binding = experiment.key.as_str();
        let record = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let document: aterm_json::Value =
            aterm_json::from_str(&record).map_err(|e| e.to_string())?;
        footprint_semantics(&document)?;
        let rows = document
            .get("comparisons")
            .and_then(aterm_json::Value::as_array)
            .ok_or(
                "memory record requires comparisons array, one evidenced bound per unchanged case",
            )?;
        let matching: Vec<_> = rows
            .iter()
            .filter(|row| {
                row.get("experiment_sha256")
                    .and_then(aterm_json::Value::as_str)
                    == Some(binding)
            })
            .collect();
        if matching.len() != 1 {
            return Err("memory record needs exactly one matching experiment".into());
        }
        let row = matching[0];
        let components = row
            .get("components")
            .and_then(aterm_json::Value::as_array)
            .ok_or("memory evidence requires component bounds")?;
        let mut bytes = 0u64;
        let mut frozen_evidence = Vec::new();
        let mut files = std::collections::BTreeMap::new();
        for name in [
            "controller",
            "app_and_keeper",
            "producers",
            "setup_cleanup_overlap",
        ] {
            let matching: Vec<_> = components
                .iter()
                .filter(|v| v.get("name").and_then(aterm_json::Value::as_str) == Some(name))
                .collect();
            if matching.len() != 1 {
                return Err(format!("memory evidence needs exactly one {name} bound"));
            }
            let component = matching[0];
            let bound = component
                .get("bound_bytes")
                .and_then(aterm_json::Value::as_u64)
                .filter(|v| *v > 0)
                .ok_or("memory component bound must be positive and known")?;
            let reason = component
                .get("bound_derivation")
                .and_then(aterm_json::Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .ok_or("memory bound derivation absent")?;
            let evidence = component
                .get("evidence_path")
                .and_then(aterm_json::Value::as_str)
                .ok_or("memory evidence path absent")?;
            let expected = component
                .get("evidence_sha256")
                .and_then(aterm_json::Value::as_str)
                .ok_or("memory evidence digest absent")?;
            let measurements = std::fs::read_to_string(evidence).map_err(|e| e.to_string())?;
            if !std::path::Path::new(evidence).is_absolute()
                || crate::measure::sha256(&measurements) != expected
            {
                return Err(format!(
                    "memory evidence for {name} is missing or changed: {reason}"
                ));
            }
            let raw_peak = validate_capture_sources(&measurements, experiment, name, &mut files)?;
            files.insert(PathBuf::from(evidence), expected.to_owned());
            let observed = measured_peak(&measurements, experiment, name)?;
            if observed != raw_peak {
                return Err(
                    "selected sizing witnesses omitted the actual raw component peak".into(),
                );
            }
            // The normal build-admission floor is a measured peak plus a fifth. This
            // minimum is not a growth proof: the component's concrete derivation may
            // demand more, and remains part of the frozen experiment evidence.
            let floor = observed
                .checked_add(observed.div_ceil(5))
                .ok_or("memory envelope overflow")?;
            if bound < floor {
                return Err("memory bound must include conservatively justified headroom of at least a fifth over its measured peak".into());
            }
            frozen_evidence.push((name.to_owned(), measurements));
            bytes = bytes.checked_add(bound).ok_or("memory bound overflow")?;
        }
        if components.len() != 4 {
            return Err("unexpected memory component".into());
        }
        Ok(Self {
            bytes,
            binding: binding.to_owned(),
            digest: crate::measure::sha256(&record),
            record,
            evidence: frozen_evidence,
            files: files.into_iter().collect(),
        })
    }
    pub(super) fn recheck(&self) -> Result<(), String> {
        for (path, digest) in &self.files {
            if aterm_update_core::sha256_file(path)? != *digest {
                return Err(format!(
                    "frozen sizing evidence changed: {}",
                    path.display()
                ));
            }
        }
        Ok(())
    }
}

// Bounds are declarations requiring review of their derivation, just as a trusted build
// record requires review of its capture. The observed peak is NOT a declaration: recompute
// it from complete component process-footprint sweeps, with exact experiment/build binding.
// Unknown member readings cannot become zero or disappear from a sum. Calibration remains
// correctness evidence for memory sizing; it grants no timing qualification.
pub(super) fn measured_peak(
    text: &str,
    experiment: &Experiment,
    component: &str,
) -> Result<u64, String> {
    let row: aterm_json::Value = aterm_json::from_str(text).map_err(|e| e.to_string())?;
    if row
        .get("experiment_sha256")
        .and_then(aterm_json::Value::as_str)
        != Some(experiment.key.as_str())
        || row.get("component").and_then(aterm_json::Value::as_str) != Some(component)
    {
        return Err("memory machine readings have wrong experiment/component binding".into());
    }
    footprint_semantics(&row)?;
    let observations = row
        .get("observations")
        .and_then(aterm_json::Value::as_array)
        .filter(|v| !v.is_empty())
        .ok_or("memory evidence needs actual component footprint observations")?;
    let mut peak = 0;
    let mut last = 0;
    let mut seen_builds = [false; 2];
    let mut origins = std::collections::HashMap::new();
    for observation in observations {
        let at = observation
            .get("monotonic_ns")
            .and_then(aterm_json::Value::as_u64)
            .filter(|n| *n > last)
            .ok_or("memory observations missing or out of order")?;
        last = at;
        let members = observation
            .get("members")
            .and_then(aterm_json::Value::as_array)
            .filter(|v| !v.is_empty())
            .ok_or("unknown component process membership")?;
        let mut identities = std::collections::HashSet::new();
        let mut total = 0u64;
        let mut one_peak = 0u64;
        let mut app = false;
        let mut controller = false;
        for member in members {
            let identity = member
                .get("identity")
                .and_then(aterm_json::Value::as_str)
                .and_then(aterm_uds::process::Identity::parse)
                .ok_or("unknown measured PID/birth/exec identity")?;
            if identity.pid <= 1 || identity.birth.0 == 0 || identity.idversion.is_none() {
                return Err("memory observation lacks a known process birth/exec identity".into());
            }
            if !identities.insert(identity) {
                return Err("duplicate component member".into());
            }
            let path = member
                .get("executable")
                .and_then(aterm_json::Value::as_str)
                .filter(|s| std::path::Path::new(s).is_absolute())
                .ok_or("unknown measured executable path")?;
            let digest = member
                .get("binary_sha256")
                .and_then(aterm_json::Value::as_str);
            if !digest.is_some_and(|s| !s.is_empty()) {
                return Err("unknown measured executable digest".into());
            }
            let source = member.get("source_sha").and_then(aterm_json::Value::as_str);
            let record = member
                .get("build_record_sha256")
                .and_then(aterm_json::Value::as_str);
            let origin = (
                path.to_owned(),
                digest.map(str::to_owned),
                source.map(str::to_owned),
                record.map(str::to_owned),
            );
            if origins
                .insert(identity, origin.clone())
                .is_some_and(|old| old != origin)
            {
                return Err("one memory PID/birth/exec identity changed executable or build/source metadata".into());
            }
            let counters = FootprintObservation::read(member)?;
            total = total
                .checked_add(counters.current)
                .ok_or("footprint overflow")?;
            one_peak = one_peak.max(counters.lifetime_max);
            controller |= Some(path) == experiment.controller_path.to_str()
                && digest == Some(experiment.controller_digest.as_str());
            if component == "app_and_keeper" {
                for (i, build) in experiment.builds.iter().enumerate() {
                    if Some(path) == build.path.to_str()
                        && digest == Some(build.digest.as_str())
                        && source == Some(build.source.as_str())
                        && record == Some(build.record_digest.as_str())
                    {
                        seen_builds[i] = true;
                        app = true;
                    }
                }
            }
        }
        if (component == "app_and_keeper" && (!app || !controller))
            || (component == "controller" && !controller)
        {
            return Err(
                "memory observation omits its exact app/keeper/controller component".into(),
            );
        }
        peak = peak.max(total.max(one_peak));
    }
    if component == "app_and_keeper" && seen_builds != [true; 2] {
        return Err(
            "memory measurements do not cover both exact frozen app builds and sources".into(),
        );
    }
    Ok(peak)
}

fn validate_capture_sources(
    text: &str,
    experiment: &Experiment,
    component: &str,
    files: &mut std::collections::BTreeMap<PathBuf, String>,
) -> Result<u64, String> {
    use aterm_json::Value;
    let doc: Value = aterm_json::from_str(text).map_err(|e| e.to_string())?;
    footprint_semantics(&doc)?;
    let captures = doc
        .get("captures")
        .and_then(Value::as_array)
        .ok_or("missing actual sizing capture sources")?;
    if captures.len() != experiment.expected_arms() {
        return Err("sizing capture does not cover every requested ABBA arm".into());
    }
    let mut generations = std::collections::HashSet::new();
    let mut witnessed = Vec::new();
    let mut raw_observations = Vec::new();
    for (ordinal, reference) in captures.iter().enumerate() {
        let path = reference
            .get("path")
            .and_then(Value::as_str)
            .filter(|p| std::path::Path::new(p).is_absolute())
            .ok_or("missing sizing capture path")?;
        let digest = reference
            .get("sha256")
            .and_then(Value::as_str)
            .ok_or("missing sizing capture hash")?;
        let bytes = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        if crate::measure::sha256(&bytes) != digest {
            return Err("sizing capture changed".into());
        }
        let capture: Value = aterm_json::from_str(&bytes).map_err(|e| e.to_string())?;
        footprint_semantics(&capture)?;
        let arm = crate::aos_cadence::ORDER[ordinal % 4].to_string();
        let build = &experiment.builds[usize::from(arm == "B")];
        if capture.get("kind").and_then(Value::as_str) != Some("functional_memory_sizing")
            || capture.get("experiment_sha256").and_then(Value::as_str)
                != Some(experiment.key.as_str())
            || capture.get("case").and_then(Value::as_str) != Some(experiment.case.as_str())
            || capture.get("ordinal").and_then(Value::as_u64) != Some(ordinal as u64)
            || capture.get("arm").and_then(Value::as_str) != Some(arm.as_str())
            || capture.get("binary_sha256").and_then(Value::as_str) != Some(build.digest.as_str())
            || capture.get("source_sha").and_then(Value::as_str) != Some(build.source.as_str())
            || capture.get("build_record_sha256").and_then(Value::as_str)
                != Some(build.record_digest.as_str())
            || capture.get("fixture_complete").and_then(Value::as_bool) != Some(true)
            || capture.get("cleanup_complete").and_then(Value::as_bool) != Some(true)
            || capture.get("timing_qualified").and_then(Value::as_bool) != Some(false)
            || !capture
                .get("functional_budget_bytes")
                .and_then(Value::as_u64)
                .is_some_and(|n| n > 0)
        {
            return Err("sizing source is partial, qualified as timing, or belongs to a different fixture/build/arm".into());
        }
        let generation = capture
            .get("generation")
            .and_then(Value::as_str)
            .filter(|v| crate::title_drift::hex(v, 32))
            .ok_or("missing sizing generation")?;
        if !generations.insert(generation.to_owned()) {
            return Err("duplicate sizing generation".into());
        }
        let raw = capture
            .get("raw_path")
            .and_then(Value::as_str)
            .filter(|p| std::path::Path::new(p).is_absolute())
            .ok_or("missing raw sizing sweeps")?;
        let raw_digest = capture
            .get("raw_sha256")
            .and_then(Value::as_str)
            .ok_or("missing raw sizing digest")?;
        if files.get(std::path::Path::new(raw)).map(String::as_str) != Some(raw_digest) {
            if aterm_update_core::sha256_file(std::path::Path::new(raw))? != raw_digest {
                return Err("raw sizing sweeps changed".into());
            }
            files.insert(PathBuf::from(raw), raw_digest.to_owned());
        }
        files.insert(PathBuf::from(path), digest.to_owned());
        for (path_key, hash_key) in [
            ("fixture_raw_path", "fixture_raw_sha256"),
            ("cast_path", "cast_sha256"),
        ] {
            let path = capture
                .get(path_key)
                .and_then(Value::as_str)
                .filter(|p| std::path::Path::new(p).is_absolute())
                .ok_or("sizing fixture raw/cast path missing")?;
            let hash = capture
                .get(hash_key)
                .and_then(Value::as_str)
                .ok_or("sizing fixture raw/cast digest missing")?;
            if aterm_update_core::sha256_file(std::path::Path::new(path))? != hash {
                return Err("sizing fixture raw/cast changed".into());
            }
            files.insert(PathBuf::from(path), hash.to_owned());
        }
        let witnesses = capture
            .get("witnesses")
            .and_then(|v| v.get(component))
            .and_then(Value::as_array)
            .filter(|v| !v.is_empty())
            .ok_or("sizing source lacks component witnesses")?;
        let stream = std::fs::read_to_string(raw).map_err(|e| e.to_string())?;
        raw_observations.extend(validate_raw_witnesses(
            &stream, &capture, component, witnesses,
        )?);
        witnessed.extend(witnesses.iter().cloned());
    }
    witnessed.sort_by_key(|v| v.get("monotonic_ns").and_then(Value::as_u64));
    if doc.get("observations").and_then(Value::as_array) != Some(&witnessed) {
        return Err("component summary omitted or changed actual frozen capture witnesses".into());
    }
    measured_peak(&aterm_json::json!({"footprint_semantics":FOOTPRINT_SEMANTICS,"experiment_sha256":experiment.key,"component":component,"observations":raw_observations}).to_string(), experiment, component)
}

fn sizing_identity(text: &str) -> Result<String, String> {
    let identity = aterm_uds::process::Identity::parse(text)
        .filter(|id| id.pid > 1 && id.birth.0 > 0 && id.idversion.is_some())
        .ok_or("unknown sizing original PID/birth/exec identity")?;
    if identity.token() != text {
        return Err("noncanonical sizing identity".into());
    }
    Ok(text.to_owned())
}
fn sizing_identity_set(
    value: Option<&aterm_json::Value>,
) -> Result<std::collections::HashSet<String>, String> {
    let rows = value
        .and_then(aterm_json::Value::as_array)
        .ok_or("missing sizing original/retirement set")?;
    let mut identities = std::collections::HashSet::new();
    for row in rows {
        let token = sizing_identity(row.as_str().ok_or("unknown sizing identity token")?)?;
        if !identities.insert(token) {
            return Err("duplicate sizing original/retirement identity".into());
        }
    }
    Ok(identities)
}

pub(super) fn sizing_live_identities(
    components: &aterm_json::Value,
) -> Result<std::collections::HashSet<String>, String> {
    let mut live = std::collections::HashSet::new();
    for members in components
        .as_object()
        .ok_or("missing sizing components")?
        .values()
    {
        for member in members
            .as_array()
            .ok_or("unknown sizing component membership")?
        {
            FootprintObservation::read(member)?;
            live.insert(sizing_identity(
                member
                    .get("identity")
                    .and_then(aterm_json::Value::as_str)
                    .ok_or("missing sizing member identity")?,
            )?);
        }
    }
    Ok(live)
}

/// Shared production completion law. It needs actual positive retirement, never a
/// missing counter, PID replacement, or disappearance of only the keeper/app roots.
pub(super) fn sizing_final_retirement(
    controller: &str,
    originals: &std::collections::HashSet<String>,
    retired: &std::collections::HashSet<String>,
    live: &std::collections::HashSet<String>,
) -> Result<(), String> {
    sizing_identity(controller)?;
    for identity in originals.iter().chain(retired.iter()) {
        sizing_identity(identity)?;
    }
    if originals.is_empty()
        || originals.contains(controller)
        || retired.contains(controller)
        || originals != retired
        || live.len() != 1
        || !live.contains(controller)
    {
        return Err(
            "sizing final completion has a surviving or unretired original owned identity".into(),
        );
    }
    Ok(())
}

fn validate_raw_witnesses(
    stream: &str,
    capture: &aterm_json::Value,
    component: &str,
    witnesses: &[aterm_json::Value],
) -> Result<Vec<aterm_json::Value>, String> {
    use aterm_json::Value;
    let mut lines = stream.lines();
    let header: Value = aterm_json::from_str(lines.next().ok_or("empty sizing stream")?)
        .map_err(|e| e.to_string())?;
    footprint_semantics(&header)?;
    footprint_semantics(capture)?;
    for key in [
        "footprint_semantics",
        "kind",
        "experiment_sha256",
        "generation",
        "functional_budget_bytes",
        "timing_qualified",
    ] {
        if header.get(key) != capture.get(key) {
            return Err("raw sizing header differs from actual capture".into());
        }
    }
    let mut matched = vec![false; witnesses.len()];
    let mut last = 0;
    let mut stopped = false;
    let mut cleanup = false;
    let mut final_retired = false;
    let mut controller = None;
    let mut originals = std::collections::HashSet::new();
    let mut retired = std::collections::HashSet::new();
    let mut observations = Vec::new();
    for line in lines {
        let row: Value = aterm_json::from_str(line).map_err(|e| e.to_string())?;
        if stopped {
            return Err("sizing stream continued after recorder completion".into());
        }
        if final_retired && row.get("event").and_then(Value::as_str) != Some("recorder_stop") {
            return Err("sizing stream continued after final retirement".into());
        }
        match row.get("event").and_then(Value::as_str) {
            Some("sweep") => {
                let at = row
                    .get("monotonic_ns")
                    .and_then(Value::as_u64)
                    .filter(|n| *n > last)
                    .ok_or("stale raw sizing sweep")?;
                last = at;
                cleanup |= row.get("phase").and_then(Value::as_str) == Some("cleanup");
                let components = row.get("components").ok_or("missing raw component sweep")?;
                let current_controller = components
                    .get("controller")
                    .and_then(Value::as_array)
                    .filter(|v| v.len() == 1)
                    .and_then(|v| v[0].get("identity"))
                    .and_then(Value::as_str)
                    .ok_or("missing actual sizing controller identity")?;
                let current_controller = sizing_identity(current_controller)?;
                if controller
                    .as_ref()
                    .is_some_and(|old| old != &current_controller)
                {
                    return Err("sizing controller identity changed".into());
                }
                controller = Some(current_controller.clone());
                let live = sizing_live_identities(components)?;
                let declared = sizing_identity_set(row.get("original_non_controller_identities"))?;
                let current_retired = sizing_identity_set(row.get("retired_original_identities"))?;
                let mut seen = live.clone();
                seen.remove(&current_controller);
                if !originals.is_subset(&declared)
                    || !seen.is_subset(&declared)
                    || !retired.is_subset(&current_retired)
                    || !current_retired.is_subset(&declared)
                    || !live.is_disjoint(&current_retired)
                {
                    return Err("sizing stream lost an original identity or treated a live identity as retired".into());
                }
                originals = declared;
                retired = current_retired;
                if row.get("phase").and_then(Value::as_str) == Some("cleanup_complete") {
                    sizing_final_retirement(&current_controller, &originals, &retired, &live)?;
                    final_retired = true;
                }
                if let Some(members) = row.get("components").and_then(|v| v.get(component)) {
                    observations.push(aterm_json::json!({"monotonic_ns":at,"members":members}));
                }
                for (i, witness) in witnesses.iter().enumerate() {
                    if witness.get("monotonic_ns").and_then(Value::as_u64) == Some(at) {
                        if witness.get("capture_generation") != capture.get("generation")
                            || witness.get("phase") != row.get("phase")
                            || witness.get("members")
                                != row.get("components").and_then(|v| v.get(component))
                        {
                            return Err(
                                "sizing witness differs from actual raw component sweep".into()
                            );
                        }
                        matched[i] = true;
                    }
                }
            }
            Some("recorder_stop")
                if row.get("error") == Some(&Value::Null)
                    && row.get("cleanup_complete").and_then(Value::as_bool) == Some(true)
                    && final_retired =>
            {
                stopped = true
            }
            _ => return Err("sizing stream has an incomplete or refused capture".into()),
        }
    }
    if !stopped || !cleanup || !final_retired || matched.iter().any(|v| !*v) {
        return Err("sizing witness lacks a completed actual raw sweep".into());
    }
    if !witnesses
        .iter()
        .any(|v| v.get("phase").and_then(Value::as_str) == Some("fixture"))
    {
        return Err("sizing capture lacks an actual completed fixture observation".into());
    }
    Ok(observations)
}

pub(super) struct Quad {
    reservation: crate::admission::ComparisonAdmission,
    builds: [Build; 2],
    case: String,
    experiment: String,
    generation: String,
    progress: Progress,
    controller_digest: String,
}

impl Quad {
    pub(super) fn reserve(experiment: &Experiment, memory: &Memory) -> Result<Self, String> {
        if memory.binding != experiment.key {
            return Err("memory evidence belongs to a different frozen experiment".into());
        }
        if aterm_update_core::sha256_file(&crate::paths::paths().exe)?
            != experiment.controller_digest
        {
            return Err("frozen experiment controller changed".into());
        }
        let builds = &experiment.builds;
        let case = experiment.case.clone();
        for build in builds {
            check_file(build)?;
        }
        let reservation = crate::admission::comparison(
            &format!("title-drift ABBA {case} {}", experiment.key),
            memory.bytes,
        )
        .map_err(|f| {
            f.msg
                .unwrap_or_else(|| "comparison admission refused".into())
        })?;
        if aterm_update_core::sha256_file(&crate::paths::paths().exe)?
            != experiment.controller_digest
        {
            return Err("frozen comparison controller changed during admission".into());
        }
        Ok(Self {
            reservation,
            builds: builds.clone(),
            case,
            experiment: experiment.key.clone(),
            generation: aterm_uds::rand::hex_token::<16>().map_err(|e| e.to_string())?,
            progress: Progress::default(),
            controller_digest: experiment.controller_digest.clone(),
        })
    }

    pub(super) fn start<'q>(
        &'q mut self,
        world: &World,
        launch: Launch,
        case: &str,
        arm: char,
        raw: &mut Raw,
    ) -> Result<Arm<'q>, String> {
        self.progress.start(case == self.case, arm)?;
        raw.note(
            "comparison_limits",
            format!(
                "original_admitted_ms={:?} need={:?} gpu_max_busy_pct={}",
                self.reservation.claim().record().timing.admitted_ms,
                self.reservation.claim().record().need,
                aterm_capacity::QUIET_GPU_BUSY
            ),
        );
        if aterm_update_core::sha256_file(&crate::paths::paths().exe)? != self.controller_digest {
            return Err("frozen comparison controller changed before arm launch".into());
        }
        crate::stop::check().map_err(|_| "comparison interrupted")?;
        // The previous arm is gone; no stale numeric credit is carried into a new launch.
        self.reservation.credit(&[]).map_err(|e| e.to_string())?;
        let footprint = self
            .reservation
            .claim()
            .comparison_memory()
            .map_err(|e| e.to_string())?;
        raw.note("prelaunch_controller_footprint_bytes", footprint);
        let build = self.builds[usize::from(arm == 'B')].clone();
        if launch.aterm != build.path {
            return Err("comparison executable differs from frozen arm".into());
        }
        check_file(&build)?;
        let generation = crate::band::Generation::fresh()?;
        let arm_generation = generation.word().to_owned();
        raw.note("comparison_binding", format!("reservation={} arm_generation={} ordinal={} case={} experiment={} claim={} admitted={:?} peak_bytes={}", self.generation, arm_generation, self.progress.next, self.case, self.experiment, self.reservation.claim().path().display(), self.reservation.claim().record().timing.admitted_ms, self.reservation.claim().record().need.mem));
        Instance::start_comparison(
            world,
            launch,
            LaunchPermit {
                owner: self,
                build,
                generation,
            },
        )
    }

    pub(super) fn complete(self) -> Result<(), String> {
        self.progress.complete()?;
        self.reservation
            .claim()
            .comparison_custody()
            .map_err(|e| e.to_string())
    }
}

pub(super) struct Arm<'q> {
    owner: &'q mut Quad,
    inst: Instance,
    build: Build,
    arm_generation: String,
    boundary: Boundary,
    finished: bool,
}

/// Opaque single-use launch authority. Only Quad::start can construct it. Instance
/// consumes it and returns a lifetime-bound Arm, never an unbound Instance.
pub(super) struct LaunchPermit<'q> {
    owner: &'q mut Quad,
    build: Build,
    generation: crate::band::Generation,
}
impl<'q> LaunchPermit<'q> {
    pub(super) fn claim(&self) -> &aterm_capacity::Claim {
        self.owner.reservation.claim()
    }
    pub(super) fn generation(&self) -> crate::band::Generation {
        self.generation.clone()
    }
    pub(super) fn bind(self, inst: Instance) -> Arm<'q> {
        Arm {
            owner: self.owner,
            inst,
            build: self.build,
            arm_generation: self.generation.word().to_owned(),
            boundary: Boundary::default(),
            finished: false,
        }
    }
}

fn check_file(build: &Build) -> Result<(), String> {
    if aterm_update_core::sha256_file(&build.path)? != build.digest
        || aterm_update_core::sha256_file(&build.record)? != build.record_digest
    {
        return Err("frozen executable or compiler record changed".into());
    }
    Ok(())
}

impl Arm<'_> {
    /// Consume the arm through setup, one measurement and one final qualification.
    /// Callbacks borrow only the immutable actual Instance. They receive no timing authority and
    /// cannot retry entry/final, retain an Instance or continue after final is attempted.
    pub(super) fn run<S, T>(
        mut self,
        raw: &mut Raw,
        setup: impl FnOnce(&Instance, &mut Raw) -> Result<S, String>,
        measure: impl FnOnce(&Instance, S, &mut Raw) -> Result<T, String>,
    ) -> Result<T, String> {
        let result = (|| {
            self.inst
                .await_socket(crate::instance::BOOT)
                .map_err(|f| f.msg.unwrap_or_else(|| "instance did not boot".into()))?;
            let setup = setup(&self.inst, raw)?;
            self.entry(raw)?;
            let value = measure(&self.inst, setup, raw)?;
            self.final_observation(raw)?;
            Ok(value)
        })();
        if let Err(why) = &result {
            raw.note("first_arm_refusal", why);
        }
        self.finish(result, raw)
    }

    fn binding(&mut self, raw: &mut Raw, phase: &str) -> Result<(), String> {
        crate::stop::check().map_err(|_| "comparison interrupted")?;
        self.owner
            .reservation
            .claim()
            .comparison_custody()
            .map_err(|e| e.to_string())?;
        if self.inst.comparison_generation() != Some(self.arm_generation.as_str()) {
            return Err("comparison arm generation changed".into());
        }
        check_file(&self.build)?;
        let (identity, _) = crate::title_drift::cpu(
            &self.inst,
            &self.build,
            self.boundary.seal.as_ref().map(|s| s.identity),
            raw,
            phase,
        )?;
        let keeper = aterm_uds::process::read_identity(self.inst.pid)
            .ok_or("owned keeper identity unavailable")?;
        if identity.idversion.is_none() || keeper.idversion.is_none() {
            return Err("actual comparison arm or keeper lacks an exec identity".into());
        }
        let keeper_path = aterm_uds::process::read_process_path(self.inst.pid)
            .ok_or("owned keeper path unavailable")?;
        let controller_digest = aterm_update_core::sha256_file(&crate::paths::paths().exe)?;
        if keeper_path != crate::paths::paths().exe
            || controller_digest != self.owner.controller_digest
        {
            return Err("actual keeper executable differs from frozen controller".into());
        }
        let actual = Seal {
            reservation: self.owner.generation.clone(),
            ordinal: self.owner.progress.next,
            generation: self
                .inst
                .comparison_generation()
                .ok_or("missing actual arm generation")?
                .to_owned(),
            path: self.inst.aterm.clone(),
            source: self.build.source.clone(),
            digest: aterm_update_core::sha256_file(&self.build.path)?,
            record: self.build.record.clone(),
            record_digest: aterm_update_core::sha256_file(&self.build.record)?,
            artifact: self.build.artifact.clone(),
            identity,
            keeper,
            keeper_path,
            controller_digest,
        };
        raw.note(&format!("arm_seal_{phase}"), format!("{actual:?}"));
        self.boundary.bind(actual)?;
        // launchd app-band processes are outside the controller's tree. Credit the exact
        // live app only after identity/path validation; original custody still owns the budget.
        self.owner
            .reservation
            .credit(&[
                u32::try_from(keeper.pid).map_err(|_| "invalid keeper pid")?,
                u32::try_from(identity.pid).map_err(|_| "invalid app pid")?,
            ])
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn entry(&mut self, raw: &mut Raw) -> Result<(), String> {
        self.boundary.can_enter()?;
        self.binding(raw, "entry_before_sample")?;
        self.owner.reservation.entry().map_err(|e| e.to_string())?;
        let observed = self
            .owner
            .reservation
            .observation(&mut |line| raw.note("entry_observation", line))
            .map_err(|e| e.to_string())?;
        self.binding(raw, "entry_after_sample")?;
        raw.note("timing_entry", format!("{observed:?}"));
        self.boundary.entered = true;
        Ok(())
    }

    fn final_observation(&mut self, raw: &mut Raw) -> Result<(), String> {
        self.boundary.can_finalize()?;
        self.binding(raw, "final_before_sample")?;
        let observed = self.boundary.final_sample(|| {
            self.owner
                .reservation
                .observation(&mut |line| raw.note("final_observation", line))
                .map_err(|e| e.to_string())
        })?;
        self.binding(raw, "final_after_sample")?;
        raw.note("timing_final", format!("{observed:?}"));
        Ok(())
    }

    fn finish<T>(mut self, result: Result<T, String>, raw: &mut Raw) -> Result<T, String> {
        let reservation = &mut self.owner.reservation;
        let build = &self.build;
        let result = complete_after_cleanup(
            &mut self.owner.progress,
            &self.boundary,
            result,
            || self.inst.stop(),
            || {
                reservation
                    .claim()
                    .comparison_custody()
                    .map_err(|e| e.to_string())?;
                check_file(build)?;
                reservation.credit(&[]).map_err(|e| e.to_string())
            },
            raw,
        );
        self.finished = true;
        result
    }
}

impl Drop for Arm<'_> {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.inst.stop();
            self.owner.progress.cancel();
        }
    }
}

/// The production transition kernel. Starting poisons admission until qualified cleanup;
/// exclusive borrowing prevents a second arm while an Arm exists, and this state rejects
/// overlap/cancellation even under injected invalid call sequences.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QuadState {
    Ready,
    Active,
    Cancelled,
    Done,
}
struct Progress {
    next: usize,
    state: QuadState,
}
impl Default for Progress {
    fn default() -> Self {
        Self {
            next: 0,
            state: QuadState::Ready,
        }
    }
}
impl Progress {
    fn start(&mut self, same_case: bool, arm: char) -> Result<(), String> {
        let allowed = self.state == QuadState::Ready
            && same_case
            && crate::aos_cadence::ORDER.get(self.next) == Some(&arm);
        self.state = if allowed {
            QuadState::Active
        } else {
            QuadState::Cancelled
        };
        if allowed {
            Ok(())
        } else {
            Err("comparison canceled, complete, wrong case or out-of-order arm".into())
        }
    }
    fn cancel(&mut self) {
        self.state = QuadState::Cancelled;
    }
    fn cleaned(&mut self) -> Result<(), String> {
        if self.state != QuadState::Active || self.next >= 4 {
            self.cancel();
            return Err("duplicate or canceled arm completion".into());
        }
        self.next += 1;
        self.state = if self.next == 4 {
            QuadState::Done
        } else {
            QuadState::Ready
        };
        Ok(())
    }
    fn complete(&self) -> Result<(), String> {
        if self.state == QuadState::Done && self.next == 4 {
            Ok(())
        } else {
            Err("comparison has no complete qualified ABBA quad".into())
        }
    }
}

/// Production cleanup/completion transition, with explicit fault injection in its tests.
/// Cleanup retains original custody; qualification then rechecks that same custody and
/// original expiry. Every error cancels the quad before returning any value to aggregation.
fn complete_after_cleanup<T>(
    progress: &mut Progress,
    boundary: &Boundary,
    result: Result<T, String>,
    cleanup: impl FnOnce() -> Result<(), String>,
    qualify: impl FnOnce() -> Result<(), String>,
    raw: &mut Raw,
) -> Result<T, String> {
    let result = (|| {
        let value = crate::title_drift::finish(result, cleanup, raw)?;
        boundary.complete()?;
        qualify()?;
        progress.cleaned()?;
        Ok(value)
    })();
    if result.is_err() {
        progress.cancel();
    }
    result
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Seal {
    reservation: String,
    ordinal: usize,
    generation: String,
    path: PathBuf,
    source: String,
    digest: String,
    record: PathBuf,
    record_digest: String,
    artifact: String,
    identity: aterm_uds::process::Identity,
    keeper: aterm_uds::process::Identity,
    keeper_path: PathBuf,
    controller_digest: String,
}

#[derive(Default)]
struct Boundary {
    seal: Option<Seal>,
    entered: bool,
    finalized: bool,
    final_attempted: bool,
}
impl Boundary {
    fn bind(&mut self, actual: Seal) -> Result<(), String> {
        if self
            .seal
            .as_ref()
            .is_some_and(|expected| *expected != actual)
        {
            return Err(
                "actual arm PID/birth/exec/file/build/source/reservation generation changed".into(),
            );
        }
        self.seal = Some(actual);
        Ok(())
    }
    fn can_enter(&self) -> Result<(), String> {
        if self.entered || self.finalized {
            Err("duplicate comparison entry".into())
        } else {
            Ok(())
        }
    }
    fn can_finalize(&self) -> Result<(), String> {
        if self.seal.is_none() || !self.entered || self.final_attempted {
            Err("missing entry or duplicate final observation".into())
        } else {
            Ok(())
        }
    }
    fn final_sample<T>(
        &mut self,
        actual_sample: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        self.can_finalize()?;
        self.final_attempted = true;
        let sample = actual_sample()?;
        self.finalized = true;
        Ok(sample)
    }
    fn complete(&self) -> Result<(), String> {
        if self.seal.is_some() && self.entered && self.finalized {
            Ok(())
        } else {
            Err("arm lacks its real entry/final qualification".into())
        }
    }
}

#[cfg(test)]
#[path = "timing_comparison_tests.rs"]
mod tests;
