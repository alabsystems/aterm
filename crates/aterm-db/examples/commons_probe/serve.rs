// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Proof observations from the real automatic Runtime, never a parallel manual
//! endpoint. Guarded live modes pause and restore live config; recovery keeps
//! the persistent policy authoritative and uses separate observer storage.
//! Receiver preflight precedes the ownership handoff. To attribute delivery to
//! this owner, write a fresh marker after its runtime-owner-active event and
//! verify that exact operation in the post-stop received rows.

use super::probe::{LocalProbe, Result, prepare_existing, received_existing_at};
use aterm_codec::hex::encode_lower;
use aterm_db::{
    replica::InventorySummary,
    runtime::{RepairRound, Runtime, Status},
};
use aterm_digest::Sha256;
use aterm_json::{Value, json};
use aterm_secrets::{client::Paths, fsx};
use std::{
    collections::BTreeSet,
    io,
    path::Path,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

const MAX_ROUNDS: usize = 4096;
const MAX_STATUSES: usize = 64;

struct ReceiverStart {
    tables_root_present: bool,
    recovery_root_present: bool,
    retained_tables: BTreeSet<[u8; 32]>,
}

impl ReceiverStart {
    /// Directory names alone need no current membership. A vault-approved,
    /// pending device can reach Runtime's ordinary enrollment path afterward.
    fn observe(paths: &Paths) -> Result<Self> {
        let primary = Self::retained_at(&paths.dir.join("commons/tables"))?;
        let recovery = Self::retained_at(&paths.dir.join("commons/recovery/tables"))?;
        let tables_root_present = primary.is_some();
        let recovery_root_present = recovery.is_some();
        let mut retained_tables = primary.unwrap_or_default();
        retained_tables.extend(recovery.unwrap_or_default());
        Ok(Self {
            tables_root_present,
            recovery_root_present,
            retained_tables,
        })
    }

    fn retained_at(root: &Path) -> Result<Option<BTreeSet<[u8; 32]>>> {
        match aterm_secrets::fsx::kind(root)? {
            aterm_secrets::fsx::Kind::Missing => return Ok(None),
            aterm_secrets::fsx::Kind::Dir => {}
            _ => return Err(io::Error::other("receiver table root is not a directory").into()),
        }
        let mut retained_tables = BTreeSet::new();
        for (count, entry) in std::fs::read_dir(root)?.enumerate() {
            if count >= 65 {
                return Err(io::Error::other("receiver retained storage exceeds its bound").into());
            }
            let name = entry?.file_name();
            if let Some(name) = name.to_str()
                && let Some(bytes) = aterm_codec::hex::decode_vec(name.as_bytes())
                && let Ok(id) = <[u8; 32]>::try_from(bytes.as_slice())
            {
                retained_tables.insert(id);
            }
        }
        Ok(Some(retained_tables))
    }

    fn value(&self) -> Value {
        json!({
            "tables_root_present":self.tables_root_present,
            "recovery_root_present":self.recovery_root_present,
            "retained_table_ids":self.retained_tables.iter().map(|id|encode_lower(id)).collect::<Vec<_>>(),
            "session_required":false,"receiver_seed_written":false,
        })
    }
}

enum Objective {
    Baseline(LocalProbe),
    Receive(ReceiverStart),
}

struct OwnerConfig<'a> {
    policy_path: &'a Path,
    policy: &'a [u8],
    /// A separate observer lets the live policy remain authoritative during
    /// persistent recovery. A copied policy must never allocate a generation.
    observer: Option<&'a Path>,
}

impl Objective {
    fn baseline(&self) -> Option<&LocalProbe> {
        match self {
            Self::Baseline(baseline) => Some(baseline),
            Self::Receive(_) => None,
        }
    }
}

#[derive(Default)]
struct Observed {
    rounds: Vec<RepairRound>,
    statuses: Vec<Value>,
    dropped: usize,
    status_dropped: usize,
    owner_active_reported: bool,
}

fn seconds_valid(seconds: u64) -> Result<()> {
    if !(1..=3600).contains(&seconds) {
        return Err(io::Error::other("serve seconds must be between 1 and 3600").into());
    }
    Ok(())
}

fn private_policy(config: &Path) -> Result<Vec<u8>> {
    let bytes = fsx::read_private(config, 1024 * 1024)?
        .ok_or_else(|| io::Error::other("explicit private configuration is missing"))?;
    if !aterm_db::runtime::enabled(config)? {
        return Err(io::Error::other("explicit private configuration disables Commons").into());
    }
    Ok(bytes.as_slice().to_vec())
}

pub(super) fn run(run: &str, seconds: u64, config: &Path) -> Result<Value> {
    seconds_valid(seconds)?;
    let policy = private_policy(config)?;
    let (paths, baseline) = prepare_existing(run, config)?;
    owner(
        run,
        seconds,
        OwnerConfig {
            policy_path: config,
            policy: &policy,
            observer: None,
        },
        paths,
        Objective::Baseline(baseline),
        || false,
    )
}

pub(super) fn run_live(run: &str, seconds: u64) -> Result<Value> {
    seconds_valid(seconds)?;
    let live = aterm_types::dirs::aterm_config_path()
        .ok_or_else(|| io::Error::other("Commons configuration path is unavailable"))?;
    // This baseline is acquired before any config mutation. PauseGuard then
    // serializes its exact private copy and pause with ordinary Settings saves.
    let (paths, baseline) = prepare_existing(run, &live)?;
    guarded_owner(
        run,
        seconds,
        &live,
        paths,
        Objective::Baseline(baseline),
        "serve-live",
    )
}

pub(super) fn receive_live(run: &str, seconds: u64) -> Result<Value> {
    seconds_valid(seconds)?;
    let live = aterm_types::dirs::aterm_config_path()
        .ok_or_else(|| io::Error::other("Commons configuration path is unavailable"))?;
    let paths = Paths::live()?;
    let before = ReceiverStart::observe(&paths)?;
    guarded_owner(
        run,
        seconds,
        &live,
        paths,
        Objective::Receive(before),
        "receive-live",
    )
}

pub(super) fn recover_live(run: &str, seconds: u64) -> Result<Value> {
    seconds_valid(seconds)?;
    let live = aterm_types::dirs::aterm_config_path()
        .ok_or_else(|| io::Error::other("Commons configuration path is unavailable"))?;
    let (policy, mode) = super::live_guard::read_config(&live)?;
    if !aterm_db::runtime::enabled(&live)? {
        return Err(io::Error::other("live Commons policy is disabled").into());
    }
    let paths = Paths::live()?;
    let before = ReceiverStart::observe(&paths)?;
    let scratch = aterm_tempfile::Builder::new()
        .prefix("cpr-")
        .tempdir_in("/tmp")?;
    let observer = scratch.path().join("observer.toml");
    let mut value = owner(
        run,
        seconds,
        OwnerConfig {
            policy_path: &live,
            policy: &policy,
            observer: Some(&observer),
        },
        paths,
        Objective::Receive(before),
        || false,
    )?;
    let unchanged = super::live_guard::read_config(&live)? == (policy, mode);
    let fields = value.as_object_mut().expect("owner report is an object");
    fields.insert("mode".into(), json!("recover-live"));
    fields.insert(
        "observer_config".into(),
        json!(observer.display().to_string()),
    );
    fields.insert("config_mutated".into(), json!(false));
    fields.insert("config_unchanged".into(), json!(unchanged));
    Ok(value)
}

fn guarded_owner(
    run: &str,
    seconds: u64,
    live: &Path,
    paths: Paths,
    objective: Objective,
    mode: &str,
) -> Result<Value> {
    let mut guard = super::live_guard::PauseGuard::pause(live)?;
    let attempted = (|| {
        let policy = private_policy(guard.private_config())?;
        owner(
            run,
            seconds,
            OwnerConfig {
                policy_path: guard.private_config(),
                policy: &policy,
                observer: None,
            },
            paths,
            objective,
            || guard.stop_requested(),
        )
    })();
    // owner() always stops/drops its Runtime before returning. Restoration must
    // therefore precede neither worker drain nor the ownership lease release.
    let backup = guard.backup_path().display().to_string();
    let restored = guard.restore();
    let mut value = match attempted {
        Ok(value) => value,
        Err(error) => json!({"run":run,"outcome":"runtime-error","error":error.to_string()}),
    };
    let fields = value.as_object_mut().expect("owner report is an object");
    fields.insert("mode".into(), json!(mode));
    fields.insert("live_config".into(), json!(live.display().to_string()));
    match restored {
        Ok(outcome) => {
            fields.insert("restore".into(), json!(format!("{outcome:?}")));
        }
        Err(error) => {
            fields.insert("outcome".into(), json!("restore-needed"));
            fields.insert("restore_error".into(), json!(error.to_string()));
            fields.insert("preserved_backup".into(), json!(backup));
        }
    }
    Ok(value)
}

fn inventory(summary: Option<InventorySummary>) -> Value {
    summary.map_or(Value::Null, |summary| {
        json!({
            "count":summary.count,"sha256":encode_lower(&summary.fingerprint),
        })
    })
}

fn classification(round: &RepairRound, baseline: Option<&LocalProbe>) -> &'static str {
    if round.cancelled {
        "cancelled"
    } else if !round.exchange_started || round.actor.is_none() || round.control.is_err() {
        "control-not-observed"
    } else if round.membership_error.is_some() {
        "membership-failed"
    } else if round.application_error.is_some() || round.application_interrupted.is_some() {
        "application-failed"
    } else if round.application_deferral.is_some() {
        "application-inventory-unavailable"
    } else if round.application_batch_deferral.is_some() {
        "application-batch-deferred"
    } else if baseline.is_some_and(|baseline| {
        round
            .application
            .as_ref()
            .is_some_and(|report| baseline.matches(report))
    }) {
        "automatic-owner-retention"
    } else if baseline.is_some() {
        "baseline-retention-not-established"
    } else if round.application.is_some() {
        "automatic-application-round"
    } else {
        "application-not-observed"
    }
}

pub(super) fn source_evidence(
    baseline: &LocalProbe,
    current: Option<&LocalProbe>,
    rounds: &[RepairRound],
    dropped: usize,
) -> Value {
    let count = |probe| {
        rounds
            .iter()
            .filter(|round| classification(round, probe) == "automatic-owner-retention")
            .count()
    };
    let prepared = count(Some(baseline));
    let observed = count(current.filter(|probe| probe.has_requested_operations()));
    let basis = if prepared > 0 {
        Some("prepared")
    } else if observed > 0 {
        Some("post-stop")
    } else {
        None
    };
    json!({
        "outcome":if basis.is_some() && dropped == 0 {"automatic-owner-retention"} else {"not-established"},
        "verified_rounds":prepared,"post_stop_verified_rounds":observed,
        "verification_basis":basis,
        "baseline_changed":current.map(|probe| probe.table != baseline.table || probe.known != baseline.known),
        "post_stop_probe":current.map(|probe|&probe.value),
    })
}

pub(super) fn deferral_value(why: Option<&aterm_db::replica::InventoryDeferral>) -> Value {
    why.map_or(Value::Null, |why| {
        json!({
            "inventory":why.inventory,"local_error":why.local_error,
            "retaining_primary_conflicts":why.retaining_primary_conflicts,
            "peer_unavailable":why.peer_unavailable,
        })
    })
}

pub(super) fn batch_deferral_value(why: Option<&aterm_db::replica::BatchDeferral>) -> Value {
    why.map_or(
        Value::Null,
        |why| json!({"batch":why.batch,"stage":why.stage,"reason":why.reason}),
    )
}

pub(super) fn application_value(report: Option<aterm_db::replica::Report>) -> Value {
    report.map_or(Value::Null, |report| json!({
        "recovery_reader":report.recovery,
        "table":report.table.map(|id|encode_lower(&id)),
        "local_known":inventory(report.local_known),"local_offered":inventory(report.local_offered),
        "peer_known":inventory(report.peer_known),"sent":report.sent,"received":report.received,
        "deferred":report.deferred,"keys_sent":report.keys_sent,"keys_received":report.keys_received,
        "conflicts_sent":report.conflicts_sent,"conflicts_received":report.conflicts_received,
    }))
}

/// A round's failure: its local words and its published cause.
fn failure_value(failure: &aterm_db::control::Failure) -> Value {
    json!({"error":failure.text,"cause":failure.cause.word()})
}

fn round_value(round: &RepairRound, baseline: Option<&LocalProbe>) -> Value {
    let control = match &round.control {
        Ok(report) => {
            json!({"sent":report.sent,"received":report.received,"deferred":report.deferred})
        }
        Err(error) => failure_value(error),
    };
    let application = application_value(round.application);
    let membership = round.membership.map_or(Value::Null, |report| {
        json!({
            "sent":report.sent,"received":report.received,"deferred":report.deferred,
        })
    });
    let handoff = match &round.handoff {
        None => Value::Null,
        Some(Ok(published)) => json!({"published":published}),
        Some(Err(error)) => failure_value(error),
    };
    json!({
        "target":round.target.map(|address|address.to_string()),
        "actor":round.actor.map(|actor|encode_lower(&actor)),
        "exchange_started":round.exchange_started,"cancelled":round.cancelled,
        "phase":round.phase.map(aterm_db::control::Phase::label),
        "completed_at_ms":round.completed_at_ms,"dropped_before":round.dropped_before,
        "classification":classification(round,baseline),"control":control,
        "membership":membership,
        "membership_error":round.membership_error.as_ref().map_or(Value::Null, failure_value),
        "application":application,
        "application_error":round.application_error.as_ref().map_or(Value::Null, failure_value),
        "application_interrupted":application_value(round.application_interrupted),
        "application_deferral":deferral_value(round.application_deferral.as_ref()),
        "application_batch_deferral":batch_deferral_value(round.application_batch_deferral.as_ref()),
        "credential_handoff":handoff,
    })
}

fn owner(
    run: &str,
    seconds: u64,
    configuration: OwnerConfig<'_>,
    paths: Paths,
    objective: Objective,
    stop_requested: impl Fn() -> bool,
) -> Result<Value> {
    let config = configuration.policy_path;
    let policy = configuration.policy;
    if stop_requested() {
        return Err(io::Error::other("serve stopped before automatic owner startup").into());
    }
    let observed = Arc::new(Mutex::new(Observed::default()));
    let statuses = observed.clone();
    let rounds = observed.clone();
    let started = Instant::now();
    // Its first LAN use waits for the LAN broker to answer, as every runtime's does.
    let runtime = start_runtime(
        paths.clone(),
        &configuration,
        move |status: Status| {
            let mut observed = statuses.lock().unwrap_or_else(|error| error.into_inner());
            let at_ms = aterm_time::unix_millis().unwrap_or(0);
            let announce =
                matches!(status, Status::Active { .. }) && !observed.owner_active_reported;
            observed.owner_active_reported |= announce;
            if observed.statuses.len() < MAX_STATUSES {
                observed
                    .statuses
                    .push(json!({"status":format!("{status:?}"),
                "at_ms":at_ms}));
            } else {
                observed.status_dropped += 1;
            }
            drop(observed);
            if announce {
                eprintln!(
                    "commons-probe runtime-owner-active pid={} at_ms={at_ms}",
                    std::process::id(),
                );
            }
        },
        move |round| {
            let mut observed = rounds.lock().unwrap_or_else(|error| error.into_inner());
            observed.dropped += round.dropped_before;
            if observed.rounds.len() < MAX_ROUNDS {
                observed.rounds.push(round);
            } else {
                observed.dropped += 1;
            }
        },
    )?;
    let until = started + Duration::from_secs(seconds);
    while Instant::now() < until && !stop_requested() {
        thread::sleep(Duration::from_millis(50));
    }
    let network = runtime.network_observation().snapshot();
    runtime.stop();
    // Reobserve after every worker stops. A live-run guard still keeps the old
    // owner paused here. A concurrent local write can change the prepared
    // inventory; an exact later round can establish that current set instead.
    let after = received_existing_at(&paths, config, run);
    let observed = observed.lock().unwrap_or_else(|error| error.into_inner());
    let verified = observed
        .rounds
        .iter()
        .filter(|round| classification(round, objective.baseline()) == "automatic-owner-retention")
        .count();
    let failed_control = observed
        .rounds
        .iter()
        .filter(|round| round.control.is_err())
        .count();
    let failed_application = observed
        .rounds
        .iter()
        .filter(|round| round.application_error.is_some())
        .count();
    let failed_membership = observed
        .rounds
        .iter()
        .filter(|round| round.membership_error.is_some())
        .count();
    let mut value = json!({
        "mode":"serve","run":run,"owner":"production-runtime","seconds":seconds,
        "elapsed_ms":started.elapsed().as_millis(),"stop_requested":stop_requested(),
        "config":config.display().to_string(),"config_sha256":encode_lower(&Sha256::digest(policy)),
        "local_probe":objective.baseline().map(|baseline|&baseline.value),"network":aterm_db::network_state::value(&network),
        "outcome":if verified > 0 && observed.dropped == 0 {"automatic-owner-retention"} else {"not-established"},
        "verified_rounds":verified,"control_failed_rounds":failed_control,"application_failed_rounds":failed_application,
        "membership_failed_rounds":failed_membership,
        "dropped_rounds":observed.dropped,"dropped_statuses":observed.status_dropped,
        "statuses":observed.statuses,"rounds":observed.rounds.iter().map(|round|round_value(round,objective.baseline())).collect::<Vec<_>>(),
        "scope":"Actual production Runtime discovery, scheduling and authenticated repair. Matching inventories include the prepared requested marker IDs before that round's positive repair. Ordinary owner repairs are automatic work; remote decryption, remote offered sets and provider readiness are not observed.",
    });
    if let Objective::Baseline(baseline) = &objective {
        let source = source_evidence(
            baseline,
            after.as_ref().ok().and_then(Option::as_ref),
            &observed.rounds,
            observed.dropped,
        );
        let fields = value.as_object_mut().expect("owner report is an object");
        fields.extend(
            source
                .as_object()
                .expect("source evidence is an object")
                .clone(),
        );
        if let Err(error) = &after {
            fields.insert("post_stop_error".into(), json!(error.to_string()));
        }
    }
    if let Objective::Receive(before) = &objective {
        // The automatic owner and all workers have stopped; the old owner is
        // still paused until guarded_owner restores its original configuration.
        let fields = value.as_object_mut().expect("owner report is an object");
        fields.insert("receiver_before".into(), before.value());
        fields.insert("scope".into(), json!("Actual production Runtime enrollment, discovery and repair; no manual dial, receiver seed, declaration or application enrollment. After stopping the Runtime, current membership and existing catalog/store reads authenticate the dedicated probe rows. Initial absence precedes the ownership handoff; a fresh post-activation marker attributes delivery to this owner. Retention alone does not prove provider readiness or global convergence."));
        match after {
            Ok(Some(probe)) => {
                let absent_before = !before.retained_tables.contains(&probe.table);
                let receiving_rounds = observed
                    .rounds
                    .iter()
                    .filter(|round| {
                        !round.cancelled
                            && round.exchange_started
                            && round.actor.is_some()
                            && round.control.is_ok()
                            && round.application_error.is_none()
                            && round.application_interrupted.is_none()
                            && round.membership_error.is_none()
                            && round.application.is_some_and(|report| {
                                report.table == Some(probe.table) && report.received > 0
                            })
                    })
                    .count();
                let outcome = if probe.has_requested_operations() {
                    if absent_before && receiving_rounds > 0 && observed.dropped == 0 {
                        "automatic-receiver-retention"
                    } else {
                        "retained-test-rows"
                    }
                } else {
                    "not-established"
                };
                fields.insert("outcome".into(), json!(outcome));
                fields.insert("probe_table_absent_before".into(), json!(absent_before));
                fields.insert("receiving_rounds".into(), json!(receiving_rounds));
                fields.insert("received_probe".into(), probe.value);
            }
            Ok(None) => {
                fields.insert("received_probe".into(), Value::Null);
                fields.insert("receive_observation".into(), json!("probe storage absent"));
            }
            Err(error) => {
                fields.insert("received_probe".into(), Value::Null);
                fields.insert("receive_error".into(), json!(error.to_string()));
            }
        }
    }
    Ok(value)
}

fn start_runtime(
    paths: Paths,
    configuration: &OwnerConfig<'_>,
    observe: impl Fn(Status) + Send + 'static,
    reports: impl Fn(RepairRound) + Send + 'static,
) -> Result<Runtime> {
    #[cfg(any(test, debug_assertions))]
    {
        Ok(Runtime::start_diagnostic(
            paths,
            configuration.policy_path.to_owned(),
            aterm_db::runtime::Diagnostic {
                observer_config: configuration
                    .observer
                    .unwrap_or(configuration.policy_path)
                    .to_owned(),
                recover_writer: configuration.observer.is_some(),
            },
            observe,
            reports,
        )?)
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        let _ = (paths, configuration.observer, observe, reports);
        Err(io::Error::other("automatic owner probes require a debug build").into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receiver_directory_preflight_needs_no_membership_or_custody_creation() {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(&root.path().join("pending"));
        let before = ReceiverStart::observe(&paths).unwrap();
        assert!(!before.tables_root_present);
        assert!(!before.recovery_root_present);
        assert!(before.retained_tables.is_empty());
        assert!(!paths.home.exists());

        let table = [42; 32];
        let retained = paths.dir.join("commons/tables").join(encode_lower(&table));
        std::fs::create_dir_all(&retained).unwrap();
        let before = ReceiverStart::observe(&paths).unwrap();
        assert!(before.tables_root_present);
        assert_eq!(before.retained_tables, BTreeSet::from([table]));
        assert!(std::fs::read_dir(&retained).unwrap().next().is_none());
        assert!(!paths.dir.join("commons/control").exists());
        assert!(!paths.dir.join("commons/table-claims").exists());

        let recovery_only = Paths::for_home(&root.path().join("recovery-only"));
        let recovery_table = [43; 32];
        let retained = recovery_only
            .dir
            .join("commons/recovery/tables")
            .join(encode_lower(&recovery_table));
        std::fs::create_dir_all(&retained).unwrap();
        let before = ReceiverStart::observe(&recovery_only).unwrap();
        assert!(!before.tables_root_present);
        assert!(before.recovery_root_present);
        assert!(
            before.retained_tables.contains(&recovery_table),
            "a retained recovery table cannot be called absent before startup"
        );
        let primary = recovery_only
            .dir
            .join("commons/tables")
            .join(encode_lower(&table));
        std::fs::create_dir_all(&primary).unwrap();
        let before = ReceiverStart::observe(&recovery_only).unwrap();
        assert!(before.tables_root_present && before.recovery_root_present);
        assert_eq!(
            before.retained_tables,
            BTreeSet::from([table, recovery_table])
        );
        assert!(!recovery_only.dir.join("commons/control").exists());
        assert!(
            !recovery_only
                .dir
                .join("commons/recovery/table-claims")
                .exists()
        );
    }
}
