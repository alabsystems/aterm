// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Proof observations from the real automatic Runtime, never a parallel manual
//! endpoint. The explicitly selected live modes pause and restore live config.
//! Receiver preflight precedes the ownership handoff. To attribute delivery to
//! this owner, write a fresh marker after its runtime-owner-active event and
//! verify that exact operation in the post-stop received rows.

use super::probe::{LocalProbe, Result, prepare_existing, received_existing_at};
use aterm_codec::hex::encode_lower;
use aterm_db::{
    replica::InventorySummary,
    runtime::{LanGate, RepairRound, Runtime, Status},
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

const MAX_ROUNDS: usize = 512;
const MAX_STATUSES: usize = 64;

struct ReceiverStart {
    tables_root_present: bool,
    retained_tables: BTreeSet<[u8; 32]>,
}

impl ReceiverStart {
    /// Directory names alone need no current membership. A vault-approved,
    /// pending device can reach Runtime's ordinary enrollment path afterward.
    fn observe(paths: &Paths) -> Result<Self> {
        let root = paths.dir.join("commons/tables");
        let entries = match std::fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Self {
                    tables_root_present: false,
                    retained_tables: BTreeSet::new(),
                });
            }
            Err(error) => return Err(error.into()),
        };
        if aterm_secrets::fsx::kind(&root)? != aterm_secrets::fsx::Kind::Dir {
            return Err(io::Error::other("receiver table root is not a directory").into());
        }
        let mut retained_tables = BTreeSet::new();
        for (count, entry) in entries.enumerate() {
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
        Ok(Self {
            tables_root_present: true,
            retained_tables,
        })
    }

    fn value(&self) -> Value {
        json!({
            "tables_root_present":self.tables_root_present,
            "retained_table_ids":self.retained_tables.iter().map(|id|encode_lower(id)).collect::<Vec<_>>(),
            "session_required":false,"receiver_seed_written":false,
        })
    }
}

enum Objective {
    Baseline(LocalProbe),
    Receive(ReceiverStart),
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
    if !(1..=180).contains(&seconds) {
        return Err(io::Error::other("serve seconds must be between 1 and 180").into());
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
        config,
        &policy,
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
            guard.private_config(),
            &policy,
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
    } else if round.application_error.is_some() {
        "application-failed"
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

fn round_value(round: &RepairRound, baseline: Option<&LocalProbe>) -> Value {
    let control = match &round.control {
        Ok(report) => {
            json!({"sent":report.sent,"received":report.received,"deferred":report.deferred})
        }
        Err(error) => json!({"error":error}),
    };
    let application = round.application.map_or(Value::Null, |report| json!({
        "table":report.table.map(|id|encode_lower(&id)),
        "local_known":inventory(report.local_known),"local_offered":inventory(report.local_offered),
        "peer_known":inventory(report.peer_known),"sent":report.sent,"received":report.received,
        "deferred":report.deferred,"keys_sent":report.keys_sent,"keys_received":report.keys_received,
        "conflicts_sent":report.conflicts_sent,"conflicts_received":report.conflicts_received,
    }));
    let membership = round.membership.map_or(Value::Null, |report| {
        json!({
            "sent":report.sent,"received":report.received,"deferred":report.deferred,
        })
    });
    json!({
        "target":round.target.map(|address|address.to_string()),
        "actor":round.actor.map(|actor|encode_lower(&actor)),
        "exchange_started":round.exchange_started,"cancelled":round.cancelled,
        "phase":round.phase.map(aterm_db::control::Phase::label),
        "completed_at_ms":round.completed_at_ms,"dropped_before":round.dropped_before,
        "classification":classification(round,baseline),"control":control,
        "membership":membership,"membership_error":round.membership_error,
        "application":application,"application_error":round.application_error,
    })
}

fn owner(
    run: &str,
    seconds: u64,
    config: &Path,
    policy: &[u8],
    paths: Paths,
    objective: Objective,
    stop_requested: impl Fn() -> bool,
) -> Result<Value> {
    if stop_requested() {
        return Err(io::Error::other("serve stopped before automatic owner startup").into());
    }
    let observed = Arc::new(Mutex::new(Observed::default()));
    let statuses = observed.clone();
    let rounds = observed.clone();
    let started = Instant::now();
    // The probe drives the LAN on purpose, so its runtime's first LAN use waits for nothing.
    let runtime = Runtime::start_with_reports(
        paths.clone(),
        config.to_owned(),
        LanGate::open(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receiver_directory_preflight_needs_no_membership_or_custody_creation() {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(&root.path().join("pending"));
        let before = ReceiverStart::observe(&paths).unwrap();
        assert!(!before.tables_root_present);
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
    }
}
