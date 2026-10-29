// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Metadata-only service observations for Network State. These reads never
//! activate an account, disclose a credential, or claim fleet convergence.

use aterm_secrets::{
    client::{self, Membership, Paths},
    commons,
};
use aterm_types::network_state::{ServiceAvailability, ServiceState, ServiceStates};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const CURRENT_FOR: Duration = Duration::from_secs(60);

/// Service facts and the original process-local start of their metadata read.
/// Cache readers retain this publication; reading it never renews its age.
#[derive(Clone, Debug)]
pub struct Publication {
    states: ServiceStates,
    sampled_at: Instant,
}

impl Publication {
    pub fn new(states: ServiceStates, sampled_at: Instant) -> Self {
        Self { states, sampled_at }
    }

    pub const fn sampled_at(&self) -> Instant {
        self.sampled_at
    }

    /// Expire each service independently, preserving all last-observed metadata.
    pub fn current_at(&self, now: Instant, wall_ms: Option<u64>) -> ServiceStates {
        let mut states = self.states.clone();
        let current = now
            .checked_duration_since(self.sampled_at)
            .is_some_and(|age| age <= CURRENT_FOR);
        for service in [&mut states.accounts, &mut states.secrets] {
            let recent = current
                && wall_ms.is_some_and(|wall| {
                    service.observed_at_ms != 0
                        && wall
                            .checked_sub(service.observed_at_ms)
                            .is_some_and(|age| age <= 60_000)
                });
            if service.availability != ServiceAvailability::Unknown && !recent {
                service.detail = format!(
                    "This service observation is no longer current. Last observed {:?}: {}",
                    service.availability, service.detail
                );
                service.availability = ServiceAvailability::Unknown;
            }
        }
        states
    }
}

/// A read-only publication handle. Busy reads provide no new sample.
#[derive(Clone)]
pub struct ObservationHandle(Arc<Mutex<Publication>>);

impl Default for ObservationHandle {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(Publication::new(
            ServiceStates::default(),
            Instant::now(),
        ))))
    }
}

impl ObservationHandle {
    pub fn try_publication(&self) -> Option<Publication> {
        match self.0.try_lock() {
            Ok(publication) => Some(publication.clone()),
            Err(std::sync::TryLockError::Poisoned(error)) => Some(error.into_inner().clone()),
            Err(std::sync::TryLockError::WouldBlock) => None,
        }
    }

    fn replace(&self, publication: Publication) {
        *self.0.lock().unwrap_or_else(|error| error.into_inner()) = publication;
    }
}

fn sample_with(
    clock: impl FnOnce() -> (Instant, Option<u64>),
    read: impl FnOnce(u64) -> ServiceStates,
) -> Publication {
    let (at, wall) = clock();
    Publication::new(read(wall.unwrap_or(0)), at)
}

/// Capture the sample start before either potentially slow service read.
pub fn observe_publication(paths: &Paths, config: &Path) -> Publication {
    sample_with(
        || (Instant::now(), aterm_time::unix_millis()),
        |wall| observe(paths, config, wall),
    )
}

/// Read service metadata off the window thread. Existing table reads use the
/// ordinary authenticated Commons custody path; no application table is created.
pub fn observe(paths: &Paths, config: &Path, now_ms: u64) -> ServiceStates {
    ServiceStates {
        accounts: accounts(paths, config, now_ms),
        secrets: secrets(paths, now_ms),
    }
}

fn state(availability: ServiceAvailability, now_ms: u64, detail: &str) -> ServiceState {
    ServiceState {
        availability,
        observed_at_ms: now_ms,
        detail: detail.into(),
        ..ServiceState::default()
    }
}

fn failed_observation(now_ms: u64, detail: &str) -> ServiceState {
    ServiceState {
        refresh_failed: true,
        ..state(ServiceAvailability::Unknown, now_ms, detail)
    }
}

fn control_busy(now_ms: u64) -> ServiceState {
    state(
        ServiceAvailability::Unknown,
        now_ms,
        "Commons custody, control history or the account table is busy; waiting to check account access",
    )
}

fn secrets(paths: &Paths, now_ms: u64) -> ServiceState {
    let status = match client::status(paths) {
        Ok(status) => status,
        Err(_) => {
            return failed_observation(now_ms, "Local vault metadata could not be read");
        }
    };
    let Some(config) = &status.config else {
        return secret_metadata(&status.membership, false, status.names.len(), now_ms);
    };
    match client::observe_materialization(paths, config, || client::status(paths)) {
        Ok(client::MaterializationObservation::Ready(held)) => secret_metadata(
            &held.inputs.membership,
            true,
            held.inputs.names.len(),
            now_ms,
        ),
        Ok(client::MaterializationObservation::Blocked(reason)) => secret_blocker(reason, now_ms),
        Err(_) => failed_observation(now_ms, "Local vault cache could not be verified"),
    }
}

fn secret_blocker(reason: client::MaterializationBlocker, now_ms: u64) -> ServiceState {
    use client::MaterializationBlocker;
    let (availability, detail) = match reason {
        MaterializationBlocker::LockUnavailable => (
            ServiceAvailability::Unknown,
            "Vault state cannot currently be observed; a writer may be active or its observation lock may be missing",
        ),
        MaterializationBlocker::Unconfigured => (
            ServiceAvailability::Unavailable,
            "No secrets vault configured",
        ),
        MaterializationBlocker::ConfigChanged => (
            ServiceAvailability::Unknown,
            "Vault configuration changed during this check; awaiting a new observation",
        ),
        MaterializationBlocker::UntrustedPin => (
            ServiceAvailability::Unavailable,
            "The configured vault signing pin is not admitted; shared secret access is unavailable",
        ),
        MaterializationBlocker::NotAdmitted => (
            ServiceAvailability::Unavailable,
            "Vault admission has not completed; waiting for authorization",
        ),
        MaterializationBlocker::Revoked => (
            ServiceAvailability::Unavailable,
            "Vault membership is revoked",
        ),
        MaterializationBlocker::SourceChanged => (
            ServiceAvailability::Unknown,
            "Local vault state does not match the configured source; awaiting a successful sync of that source",
        ),
        MaterializationBlocker::LocalFilesIncomplete => (
            ServiceAvailability::Unknown,
            "Local vault cache or delivered files are incomplete or no longer match; awaiting a successful sync",
        ),
    };
    state(availability, now_ms, detail)
}

fn secret_metadata(
    membership: &Membership,
    ready: bool,
    entries: usize,
    now_ms: u64,
) -> ServiceState {
    match membership {
        Membership::Unconfigured => state(
            ServiceAvailability::Unavailable,
            now_ms,
            "No secrets vault configured",
        ),
        Membership::Pending => state(
            ServiceAvailability::Unavailable,
            now_ms,
            "Vault admission is pending",
        ),
        Membership::Revoked { .. } => state(
            ServiceAvailability::Unavailable,
            now_ms,
            "Vault membership is revoked",
        ),
        Membership::Plain => state(
            ServiceAvailability::Unknown,
            now_ms,
            "Local unsigned source; shared vault access is unverified",
        ),
        Membership::In { seq, heard } => {
            let mut result = if ready && entries > 0 {
                state(
                    ServiceAvailability::Cached,
                    now_ms,
                    &format!(
                        "{entries} authorized manifest entries; file delivery, login readiness and remote currency are not established by this read"
                    ),
                )
            } else if ready {
                state(
                    ServiceAvailability::Unknown,
                    now_ms,
                    "Vault cache is verified but its manifest contains no entries",
                )
            } else {
                state(
                    ServiceAvailability::Unknown,
                    now_ms,
                    "Vault member; local cache is busy, incomplete, or no longer matches its source",
                )
            };
            result.revision = Some(format!("vault {seq}"));
            result.last_success_ms = heard
                .checked_mul(1000)
                .filter(|at| *at > 0 && *at <= now_ms);
            result
        }
    }
}

fn accounts(paths: &Paths, config: &Path, now_ms: u64) -> ServiceState {
    // This observer runs off the window thread. Give an active local batch time
    // to release custody, then obtain fresh authorization within the same wait.
    accounts_until(
        paths,
        config,
        now_ms,
        std::time::Instant::now() + Duration::from_secs(2),
    )
}

fn accounts_until(
    paths: &Paths,
    config: &Path,
    now_ms: u64,
    deadline: std::time::Instant,
) -> ServiceState {
    match commons::observe(paths) {
        Ok(commons::Observation::Unavailable(reason)) => {
            let availability = if matches!(reason, commons::Unavailable::Busy) {
                ServiceAvailability::Unknown
            } else {
                ServiceAvailability::Unavailable
            };
            return state(availability, now_ms, reason.explanation());
        }
        Err(_) => {
            return failed_observation(now_ms, "Commons access could not be verified");
        }
        Ok(commons::Observation::Visible(_)) => {}
    }
    // Do not let observing an uninitialized service create its catalog or table.
    match paths.dir.join("commons/tables").try_exists() {
        Ok(false) => {
            return state(
                ServiceAvailability::Unavailable,
                now_ms,
                "Commons table storage not retained locally; harness rosters are separate",
            );
        }
        Err(_) => {
            return failed_observation(now_ms, "Retained catalog storage could not be inspected");
        }
        Ok(true) => {}
    }
    let bootstrap = match commons::bootstrap::Bootstrap::cached_until(paths, deadline) {
        Ok(Some(bootstrap)) => bootstrap,
        Ok(None) => {
            return state(
                ServiceAvailability::Unknown,
                now_ms,
                "Commons custody could not be verified",
            );
        }
        Err(error)
            if matches!(
                error.kind(),
                aterm_secrets::ErrorKind::JournalBusy | aterm_secrets::ErrorKind::CustodyBusy
            ) =>
        {
            return control_busy(now_ms);
        }
        Err(_) => {
            return failed_observation(now_ms, "Commons custody could not be verified");
        }
    };
    let directory = crate::catalog::Directory::new(paths, bootstrap.trust());
    let descriptor = match crate::catalog::Descriptor::application(
        bootstrap.trust(),
        "accounts",
        "proposals",
        1,
    ) {
        Ok(descriptor) => descriptor,
        Err(_) => {
            return failed_observation(now_ms, "Account catalog identity is unavailable");
        }
    };
    let session = match crate::table::Session::observe(&bootstrap) {
        Ok(session) => session,
        Err(_) => {
            return failed_observation(now_ms, "Account access could not be verified");
        }
    };
    #[cfg(test)]
    tests::observed();
    // The session and table snapshots read beside writers. A newer control
    // history or table witness makes this observation stale; read it afresh.
    // Every changed claim still refuses, and damaged custody remains a failure.
    let moved = || {
        crate::table::Session::observe(&bootstrap)
            .and_then(|now| now.observation())
            .ok()
            != session.observation().ok()
    };
    // A retained enrollment claim can outlive its data directory.
    let catalog = match directory.observe_with_config(&session, config) {
        Ok(catalog) => catalog,
        Err(error) if error.is_busy() || moved() => return control_busy(now_ms),
        Err(_) => {
            return failed_observation(now_ms, "Shared catalog metadata could not be read");
        }
    };
    if !catalog
        .entries
        .get(&descriptor.scope.digest())
        .is_some_and(|entry| {
            entry.state == crate::catalog::State::Subscribed
                && entry
                    .declarations
                    .iter()
                    .all(|d| d.descriptor == descriptor)
        })
    {
        return state(
            ServiceAvailability::Unavailable,
            now_ms,
            "Account table is excluded, conflicting, or unavailable",
        );
    }
    let store = match directory.path(descriptor.scope).and_then(|path| {
        crate::table::store::Store::observe_if_exists(&session, &path, descriptor.scope)
    }) {
        Ok(Some(store)) => store,
        Ok(None) => {
            return state(
                ServiceAvailability::Unavailable,
                now_ms,
                "Account proposals not retained locally; harness rosters are separate",
            );
        }
        Err(error) if error.is_busy() || moved() => return control_busy(now_ms),
        Err(_) => {
            return failed_observation(now_ms, "Retained account storage could not be opened");
        }
    };
    #[cfg(test)]
    tests::store_observed();
    let snapshot =
        match crate::accounts::proposals::Proposals::snapshot_from_store(&session, &store) {
            Ok(snapshot) => snapshot,
            Err(error) if error.is_busy() || moved() => return control_busy(now_ms),
            Err(_) => {
                return failed_observation(now_ms, "Retained account records could not be read");
            }
        };
    let counts = account_counts(&snapshot.cells);
    let mut result = state(
        ServiceAvailability::Cached,
        now_ms,
        &format!(
            "{} account proposals, {} deleted, {} unresolved, {} stopped; catalog access does not prove login readiness or remote currency",
            counts.proposals, counts.deleted, counts.unresolved, counts.stopped,
        ),
    );
    result.last_success_ms = Some(now_ms);
    result.revision = Some(format!(
        "{} operations retained; {} withheld",
        snapshot.retained, snapshot.withheld
    ));
    result
}

#[derive(Debug, Default, PartialEq, Eq)]
struct AccountCounts {
    proposals: usize,
    deleted: usize,
    unresolved: usize,
    stopped: usize,
}

fn account_counts(
    cells: &std::collections::BTreeMap<(String, String), crate::database::Cell>,
) -> AccountCounts {
    let mut counts = AccountCounts::default();
    for ((collection, _), cell) in cells {
        if collection != "accounts" {
            continue;
        }
        if cell.stopped {
            counts.stopped += 1;
        } else if cell.candidates.len() != 1 {
            counts.unresolved += 1;
        } else if cell.candidates[0].value.is_null() {
            counts.deleted += 1;
        } else {
            counts.proposals += 1;
        }
    }
    counts
}

#[derive(Default)]
struct Stop {
    requested: Mutex<bool>,
    changed: Condvar,
}

/// A process-owned worker; window reads only its immutable metadata snapshots.
pub struct Runtime {
    shared: ObservationHandle,
    stop: Arc<Stop>,
    worker: Option<JoinHandle<()>>,
}

impl Runtime {
    pub fn start(paths: Paths, config: PathBuf) -> crate::Result<Self> {
        let shared = ObservationHandle::default();
        let stop = Arc::new(Stop::default());
        let publish = shared.clone();
        let stopping = stop.clone();
        let worker = thread::Builder::new()
            .name("network-services".into())
            .spawn(move || {
                loop {
                    if *stopping.requested.lock().unwrap_or_else(|p| p.into_inner()) {
                        break;
                    }
                    let publication = observe_publication(&paths, &config);
                    publish.replace(publication);
                    let requested = stopping.requested.lock().unwrap_or_else(|p| p.into_inner());
                    let _ = stopping.changed.wait_timeout_while(
                        requested,
                        Duration::from_secs(10),
                        |stop| !*stop,
                    );
                }
            })?;
        Ok(Self {
            shared,
            stop,
            worker: Some(worker),
        })
    }

    pub fn observation(&self) -> ObservationHandle {
        self.shared.clone()
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        *self
            .stop
            .requested
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = true;
        self.stop.changed.notify_all();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sampled_states(wall: u64) -> ServiceStates {
        let service = ServiceState {
            availability: ServiceAvailability::Cached,
            observed_at_ms: wall,
            last_success_ms: Some(wall),
            revision: Some("public revision".into()),
            detail: "retained metadata".into(),
            refresh_failed: true,
        };
        ServiceStates {
            accounts: service.clone(),
            secrets: service,
        }
    }

    #[test]
    fn publication_age_cannot_be_renewed_by_wall_rollback_or_a_late_first_read() {
        let at = Instant::now();
        let states = sampled_states(100_000);
        let publication = Publication::new(states.clone(), at);
        assert_eq!(
            publication.current_at(at + Duration::from_secs(60), Some(160_000)),
            states
        );
        for (age, wall) in [
            (61, Some(161_001)),
            (62, Some(130_000)),
            (120, Some(110_000)),
            (0, None),
            (0, Some(99_999)),
            (0, Some(0)),
        ] {
            let expired = publication.current_at(at + Duration::from_secs(age), wall);
            for service in [&expired.accounts, &expired.secrets] {
                assert_eq!(service.availability, ServiceAvailability::Unknown);
                assert_eq!(service.observed_at_ms, 100_000);
                assert_eq!(service.last_success_ms, Some(100_000));
                assert_eq!(service.revision, states.accounts.revision);
                assert!(service.refresh_failed);
            }
        }
        assert_eq!(
            publication
                .current_at(at - Duration::from_secs(1), Some(100_000))
                .accounts
                .availability,
            ServiceAvailability::Unknown
        );
        assert_eq!(
            Publication::new(sampled_states(0), at)
                .current_at(at, Some(100_000))
                .accounts
                .availability,
            ServiceAvailability::Unknown
        );
        assert_eq!(publication.states, states);
    }

    #[test]
    fn sample_clock_is_taken_before_the_metadata_reader_runs() {
        let at = Instant::now();
        let now = std::cell::Cell::new(at);
        let publication = sample_with(
            || (now.get(), Some(100_000)),
            |wall| {
                now.set(at + Duration::from_secs(70));
                sampled_states(wall)
            },
        );
        assert_eq!(publication.sampled_at(), at);
        assert_eq!(
            publication
                .current_at(now.get(), Some(110_000))
                .secrets
                .availability,
            ServiceAvailability::Unknown
        );
    }

    #[test]
    fn busy_and_repeated_handle_reads_preserve_the_original_publication() {
        let at = Instant::now();
        let handle = ObservationHandle::default();
        handle.replace(Publication::new(sampled_states(100_000), at));
        let cached = handle.try_publication().unwrap();
        let held = handle.0.lock().unwrap();
        assert!(handle.try_publication().is_none());
        assert_eq!(
            cached
                .current_at(at + Duration::from_secs(61), Some(120_000))
                .accounts
                .availability,
            ServiceAvailability::Unknown
        );
        drop(held);
        assert_eq!(handle.try_publication().unwrap().sampled_at(), at);
        handle.replace(Publication::new(
            sampled_states(120_000),
            at + Duration::from_secs(61),
        ));
        assert_eq!(
            handle
                .try_publication()
                .unwrap()
                .current_at(at + Duration::from_secs(61), Some(120_000))
                .accounts
                .availability,
            ServiceAvailability::Cached
        );
    }

    #[test]
    fn publication_normalizes_each_service_without_erasing_the_other() {
        let at = Instant::now();
        let mut states = sampled_states(100_000);
        states.accounts.observed_at_ms = 40_000;
        let publication = Publication::new(states, at);
        let current = publication.current_at(at + Duration::from_millis(1), Some(100_001));
        assert_eq!(current.accounts.availability, ServiceAvailability::Unknown);
        assert_eq!(current.secrets.availability, ServiceAvailability::Cached);
    }

    #[test]
    fn vault_blockers_never_claim_usable_entries_or_reuse_a_previous_revision() {
        use client::MaterializationBlocker;
        for (reason, availability) in [
            (
                MaterializationBlocker::LockUnavailable,
                ServiceAvailability::Unknown,
            ),
            (
                MaterializationBlocker::Unconfigured,
                ServiceAvailability::Unavailable,
            ),
            (
                MaterializationBlocker::ConfigChanged,
                ServiceAvailability::Unknown,
            ),
            (
                MaterializationBlocker::UntrustedPin,
                ServiceAvailability::Unavailable,
            ),
            (
                MaterializationBlocker::NotAdmitted,
                ServiceAvailability::Unavailable,
            ),
            (
                MaterializationBlocker::Revoked,
                ServiceAvailability::Unavailable,
            ),
            (
                MaterializationBlocker::SourceChanged,
                ServiceAvailability::Unknown,
            ),
            (
                MaterializationBlocker::LocalFilesIncomplete,
                ServiceAvailability::Unknown,
            ),
        ] {
            let observation = secret_blocker(reason, 2000);
            assert_eq!(observation.availability, availability);
            assert_eq!(observation.observed_at_ms, 2000);
            assert_eq!(observation.last_success_ms, None);
            assert_eq!(observation.revision, None);
            assert!(!observation.refresh_failed);
        }
    }

    #[test]
    fn account_counts_separate_deletions_conflicts_and_stops_from_proposals() {
        use crate::database::{Candidate, Cell};
        let candidate = |value| Candidate {
            operation: [0; 32],
            value,
        };
        let cells = [
            (
                "accounts",
                "proposal",
                false,
                vec![candidate(aterm_json::json!({}))],
            ),
            (
                "accounts",
                "deleted",
                false,
                vec![candidate(aterm_json::Value::Null)],
            ),
            ("accounts", "empty", false, vec![]),
            (
                "accounts",
                "conflict",
                false,
                vec![
                    candidate(aterm_json::json!({})),
                    candidate(aterm_json::Value::Null),
                ],
            ),
            (
                "accounts",
                "stopped",
                true,
                vec![candidate(aterm_json::json!({}))],
            ),
            (
                "email-addresses",
                "contact",
                false,
                vec![candidate(aterm_json::json!({}))],
            ),
        ]
        .into_iter()
        .map(|(collection, key, stopped, candidates)| {
            (
                (collection.into(), key.into()),
                Cell {
                    stopped,
                    candidates,
                },
            )
        })
        .collect();
        assert_eq!(
            account_counts(&cells),
            AccountCounts {
                proposals: 1,
                deleted: 1,
                unresolved: 2,
                stopped: 1,
            }
        );
        assert_eq!(
            account_counts(&Default::default()),
            AccountCounts::default()
        );
    }

    fn table_storage_bytes(root: &Path) -> std::collections::BTreeMap<PathBuf, Option<Vec<u8>>> {
        fn visit(
            root: &Path,
            path: &Path,
            entries: &mut std::collections::BTreeMap<PathBuf, Option<Vec<u8>>>,
        ) {
            for entry in std::fs::read_dir(path).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                let key = path.strip_prefix(root).unwrap().to_path_buf();
                if entry.file_type().unwrap().is_dir() {
                    entries.insert(key, None);
                    visit(root, &path, entries);
                } else {
                    entries.insert(key, Some(std::fs::read(&path).unwrap()));
                }
            }
        }
        let mut entries = std::collections::BTreeMap::new();
        visit(root, root, &mut entries);
        // Opening authenticated custody may refresh its own journal sidecars.
        // The status read must not enroll or alter any application table.
        entries.retain(|path, _| {
            path.starts_with("commons/tables") || path.starts_with("commons/table-claims")
        });
        entries
    }

    #[test]
    fn observing_empty_or_partial_catalog_storage_never_enrolls_it() {
        use aterm_secrets::commons::{bootstrap::Bootstrap, roster::Role};

        for partial in [false, true] {
            let lab = crate::table::tests::Lab::new(&[Role::Writer]);
            let paths = &lab.paths[0];
            let bootstrap = Bootstrap::cached(paths).unwrap().unwrap();
            let directory = crate::catalog::Directory::new(paths, bootstrap.trust());
            let account = crate::catalog::Descriptor::application(
                bootstrap.trust(),
                "accounts",
                "proposals",
                1,
            )
            .unwrap();
            let catalog_path = directory.path(directory.catalog_scope()).unwrap();
            aterm_journal::create_private_dir_durable(&paths.dir.join("commons/tables")).unwrap();
            for scope in [directory.catalog_scope(), account.scope] {
                aterm_journal::create_private_dir_durable(&directory.path(scope).unwrap()).unwrap();
            }
            if partial {
                aterm_journal::write_private_durable(
                    &catalog_path,
                    "table-astream",
                    b"interrupted publication",
                )
                .unwrap();
            }
            let before = table_storage_bytes(&paths.dir);
            let observation = accounts(paths, &paths.home.join("config.toml"), 1000);
            let after = table_storage_bytes(&paths.dir);
            assert!(after == before, "table storage mutated: partial={partial}");
            assert_eq!(observation.availability, ServiceAvailability::Unknown);
            assert!(observation.refresh_failed);
        }
    }

    #[test]
    fn malformed_service_metadata_warns_and_a_later_read_clears_the_warning() {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(&root.path().join("home"));
        let config = paths.home.join(".config/aterm/aterm.toml");
        std::fs::create_dir_all(paths.dir.parent().unwrap()).unwrap();
        aterm_journal::create_private_dir_durable(&paths.dir).unwrap();
        aterm_journal::write_private_durable(&paths.dir, "source.toml", b"[broken").unwrap();
        let failed = observe(&paths, &config, 1000);
        for service in [&failed.accounts, &failed.secrets] {
            assert_eq!(service.availability, ServiceAvailability::Unknown);
            assert!(service.refresh_failed);
            assert_eq!(service.observed_at_ms, 1000);
            assert_eq!(service.last_success_ms, None);
            assert_eq!(service.revision, None);
            assert!(!service.detail.contains("[broken"));
        }
        std::fs::remove_file(paths.dir.join("source.toml")).unwrap();
        let recovered = observe(&paths, &config, 2000);
        for service in [&recovered.accounts, &recovered.secrets] {
            assert_eq!(service.availability, ServiceAvailability::Unavailable);
            assert!(!service.refresh_failed);
            assert_eq!(service.observed_at_ms, 2000);
        }
    }

    #[test]
    fn membership_is_not_usable_cache_or_remote_currency() {
        let membership = Membership::In { seq: 66, heard: 12 };
        let missing = secret_metadata(&membership, false, 11, 15_000);
        assert_eq!(missing.availability, ServiceAvailability::Unknown);
        let cached = secret_metadata(&membership, true, 11, 15_000);
        assert_eq!(cached.availability, ServiceAvailability::Cached);
        assert_eq!(cached.last_success_ms, Some(12_000));
        assert!(!cached.refresh_failed);
        assert_eq!(
            secret_metadata(&membership, true, 0, 15_000).availability,
            ServiceAvailability::Unknown
        );
        assert_eq!(
            secret_metadata(&membership, true, 11, 10_000).last_success_ms,
            None
        );
    }

    #[test]
    fn unavailable_or_unsigned_membership_never_advertises_access() {
        for membership in [
            Membership::Unconfigured,
            Membership::Pending,
            Membership::Revoked { seq: 9 },
            Membership::Plain,
        ] {
            let result = secret_metadata(&membership, true, 11, 15_000);
            assert!(!matches!(
                result.availability,
                ServiceAvailability::Cached | ServiceAvailability::Current
            ));
            assert_eq!(result.last_success_ms, None);
        }
    }

    #[test]
    fn observing_unconfigured_services_does_not_create_custody() {
        let root = aterm_tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let paths = Paths::for_home(&home);
        let result = observe(&paths, &home.join(".config/aterm/aterm.toml"), 1000);
        assert_eq!(
            result.accounts.availability,
            ServiceAvailability::Unavailable
        );
        assert_eq!(
            result.secrets.availability,
            ServiceAvailability::Unavailable
        );
        assert!(!home.exists());
    }

    #[test]
    fn account_observation_reads_existing_records_without_activating_or_exposing_them() {
        use crate::{accounts::proposals::Proposals, database::Change};
        use aterm_secrets::commons::roster::Role;

        let lab = crate::table::tests::Lab::new(&[Role::Writer]);
        let paths = &lab.paths[0];
        let config = paths.home.join("config.toml");
        assert_eq!(
            accounts(paths, &config, 1000).availability,
            ServiceAvailability::Unavailable
        );
        // Retain a real declaration and let discovery reserve its table before
        // enrollment. This is still unavailable account storage, not damage.
        let bootstrap = lab.bootstrap(0);
        let session = crate::table::Session::open(&bootstrap).unwrap();
        let directory = crate::catalog::Directory::new(paths, bootstrap.trust());
        let descriptor =
            crate::catalog::Descriptor::application(bootstrap.trust(), "accounts", "proposals", 1)
                .unwrap();
        aterm_journal::create_private_dir_durable(&paths.dir.join("commons/tables")).unwrap();
        let scope = directory.catalog_scope();
        let mut catalog =
            crate::table::store::Store::join(&session, &directory.path(scope).unwrap(), scope)
                .unwrap();
        catalog
            .commit_catalog(&session, &descriptor.encode().unwrap())
            .unwrap();
        drop(catalog);
        let subscribed = directory
            .subscriptions(&session, &crate::catalog::policy::Policy::default())
            .unwrap();
        assert!(subscribed.contains_key(&descriptor.scope.digest()));
        drop(session);
        drop(bootstrap);
        let reserved = table_storage_bytes(&paths.dir);
        let observation = accounts(paths, &config, 1500);
        assert_eq!(
            observation.availability,
            ServiceAvailability::Unavailable,
            "{observation:?}"
        );
        assert!(!observation.refresh_failed);
        assert_eq!(observation.last_success_ms, None);
        assert_eq!(table_storage_bytes(&paths.dir), reserved);

        let proposals = Proposals::at(paths.clone());
        proposals.initialize().unwrap();
        proposals.write([1; 16], vec![Change {
            collection: "accounts".into(),
            key: "private-account-label".into(),
            value: Some(aterm_json::json!({"provider":"claude", "method":"oauth", "email":"private@example.test"})),
        }]).unwrap();
        let before = proposals.snapshot().unwrap().operations.len();
        let observation = accounts(paths, &config, 2000);
        assert_eq!(observation.availability, ServiceAvailability::Cached);
        assert_eq!(observation.last_success_ms, Some(2000));
        assert!(!observation.detail.contains("private"));
        assert_eq!(proposals.snapshot().unwrap().operations.len(), before);
    }

    #[test]
    fn account_observation_reads_control_history_beside_a_held_writer() {
        use aterm_secrets::commons::{journal::Journal, roster::Role};
        let lab = crate::table::tests::Lab::new(&[Role::Writer]);
        let paths = &lab.paths[0];
        let config = paths.home.join("config.toml");
        crate::accounts::proposals::Proposals::at(paths.clone())
            .initialize()
            .unwrap();
        let bootstrap = lab.bootstrap(0);
        let held = Journal::open_existing(bootstrap.journal_dir(), bootstrap.trust()).unwrap();
        assert!(crate::table::Session::open_until(&bootstrap, std::time::Instant::now()).is_err());
        let observed = crate::table::Session::observe(&bootstrap).unwrap();
        assert!(
            observed.writer().is_err(),
            "an observation grants no table writing"
        );
        let read = accounts(paths, &config, 1000);
        assert_eq!(read.availability, ServiceAvailability::Cached);
        assert_eq!(read.last_success_ms, Some(1000));
        drop(observed);
        drop(held);
        assert!(
            crate::table::Session::open(&bootstrap)
                .unwrap()
                .writer()
                .is_ok()
        );
        assert_eq!(
            accounts(paths, &config, 2000).availability,
            ServiceAvailability::Cached
        );
    }

    thread_local! {
        /// Run once [`accounts`] has observed its session.
        static OBSERVED: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
            const { std::cell::RefCell::new(None) };
        /// Run once the account table's witnessed snapshot has opened.
        static STORE_OBSERVED: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
            const { std::cell::RefCell::new(None) };
    }
    pub(super) fn observed() {
        let hook = OBSERVED.with(|hook| hook.borrow_mut().take());
        if let Some(mut hook) = hook {
            hook();
        }
    }
    pub(super) fn store_observed() {
        let hook = STORE_OBSERVED.with(|hook| hook.borrow_mut().take());
        if let Some(mut hook) = hook {
            hook();
        }
    }
    fn account(key: &str) -> crate::database::Change {
        crate::database::Change {
            collection: "accounts".into(),
            key: key.into(),
            value: Some(aterm_json::json!({"provider":"claude", "method":"oauth"})),
        }
    }

    #[test]
    fn account_observation_waits_on_custody_but_reads_beside_table_writers() {
        use crate::accounts::proposals::Proposals;
        use aterm_secrets::commons::roster::Role;
        let lab = crate::table::tests::Lab::new(&[Role::Writer]);
        let paths = &lab.paths[0];
        let config = paths.home.join("config.toml");
        let proposals = Proposals::at(paths.clone());
        proposals.initialize().unwrap();
        proposals.write([1; 16], vec![account("one")]).unwrap();
        let bootstrap = lab.bootstrap(0);

        // Another custody observation holds commons.lock past the wait.
        let custody = aterm_uds::private_file::PrivateLock::take(
            &paths.dir,
            "commons.lock",
            std::time::Duration::ZERO,
            "held",
        )
        .unwrap();
        let busy = accounts(paths, &config, 1000);
        assert_eq!(busy.availability, ServiceAvailability::Unknown, "{busy:?}");
        assert!(!busy.refresh_failed);
        drop(custody);

        // A writer in either journal leaves its witnessed account prefix readable.
        let directory = crate::catalog::Directory::new(paths, bootstrap.trust());
        let scope =
            crate::catalog::Descriptor::application(bootstrap.trust(), "accounts", "proposals", 1)
                .unwrap()
                .scope;
        for scope in [directory.catalog_scope(), scope] {
            let session = crate::table::Session::open(&bootstrap).unwrap();
            let table =
                crate::table::store::Store::open(&session, &directory.path(scope).unwrap(), scope)
                    .unwrap();
            drop(session);
            let read = accounts(paths, &config, 2000);
            assert_eq!(read.availability, ServiceAvailability::Cached, "{read:?}");
            assert_eq!(read.last_success_ms, Some(2000));
            assert!(read.revision.is_some());
            assert!(!read.refresh_failed);
            drop(table);
        }
        assert_eq!(
            accounts(paths, &config, 3000).availability,
            ServiceAvailability::Cached
        );
    }

    #[test]
    fn an_account_append_during_observation_is_busy_then_readable() {
        use crate::accounts::proposals::Proposals;
        use aterm_secrets::commons::roster::Role;
        let lab = std::rc::Rc::new(crate::table::tests::Lab::new(&[Role::Writer]));
        let paths = lab.paths[0].clone();
        let config = paths.home.join("config.toml");
        let proposals = Proposals::at(paths.clone());
        proposals.initialize().unwrap();
        proposals.write([1; 16], vec![account("one")]).unwrap();
        let moved = std::rc::Rc::clone(&lab);
        STORE_OBSERVED.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                Proposals::at(moved.paths[0].clone())
                    .write([2; 16], vec![account("two")])
                    .unwrap();
            }));
        });
        let raced = accounts(&paths, &config, 1000);
        assert_eq!(
            raced.availability,
            ServiceAvailability::Unknown,
            "{raced:?}"
        );
        assert!(!raced.refresh_failed, "a real append is not failed storage");
        assert_eq!(proposals.snapshot().unwrap().operations.len(), 2);
        let next = accounts(&paths, &config, 2000);
        assert_eq!(next.availability, ServiceAvailability::Cached);
        assert_eq!(next.last_success_ms, Some(2000));
        assert!(!next.refresh_failed);
    }

    #[test]
    fn account_records_landed_under_newer_control_history_read_as_busy_not_failed() {
        use crate::accounts::proposals::Proposals;
        use aterm_secrets::commons::roster::Role;
        let lab = std::rc::Rc::new(crate::table::tests::Lab::new(&[Role::Writer]));
        let paths = lab.paths[0].clone();
        let config = paths.home.join("config.toml");
        Proposals::at(paths.clone()).initialize().unwrap();
        // Between this read's session and its table read, control history
        // moves on and a record lands under it, as a peer repair lands them.
        let moved = std::rc::Rc::clone(&lab);
        OBSERVED.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                moved.rotate(&[Role::Writer]);
                Proposals::at(moved.paths[0].clone())
                    .write([2; 16], vec![account("two")])
                    .unwrap();
            }));
        });
        let raced = accounts(&paths, &config, 1000);
        assert!(OBSERVED.with(|hook| hook.borrow().is_none()));
        assert_eq!(
            raced.availability,
            ServiceAvailability::Unknown,
            "{raced:?}"
        );
        assert!(!raced.refresh_failed, "{raced:?}");
        assert_eq!(
            accounts(&paths, &config, 2000).availability,
            ServiceAvailability::Cached
        );
    }

    #[test]
    fn a_catalog_record_landed_under_newer_control_history_reads_as_busy_not_failed() {
        use crate::accounts::proposals::Proposals;
        use aterm_secrets::commons::roster::Role;
        let lab = std::rc::Rc::new(crate::table::tests::Lab::new(&[Role::Writer]));
        let paths = lab.paths[0].clone();
        let config = paths.home.join("config.toml");
        Proposals::at(paths.clone()).initialize().unwrap();
        // Between this read's session and its catalog read, control history
        // moves on and a declaration lands in the catalog under it.
        let moved = std::rc::Rc::clone(&lab);
        OBSERVED.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                moved.rotate(&[Role::Writer]);
                let bootstrap = moved.bootstrap(0);
                let session = crate::table::Session::open(&bootstrap).unwrap();
                let descriptor =
                    crate::catalog::Descriptor::application(bootstrap.trust(), "notes", "pages", 1)
                        .unwrap();
                crate::catalog::Directory::new(&moved.paths[0], bootstrap.trust())
                    .publish(&session, &descriptor)
                    .unwrap();
            }));
        });
        let raced = accounts(&paths, &config, 1000);
        assert!(OBSERVED.with(|hook| hook.borrow().is_none()));
        assert_eq!(
            raced.availability,
            ServiceAvailability::Unknown,
            "{raced:?}"
        );
        assert!(!raced.refresh_failed, "{raced:?}");
        assert_eq!(
            accounts(&paths, &config, 2000).availability,
            ServiceAvailability::Cached
        );
    }

    #[test]
    fn a_damaged_account_store_under_unchanged_control_history_still_fails() {
        use crate::accounts::proposals::Proposals;
        use aterm_secrets::commons::roster::Role;
        let lab = crate::table::tests::Lab::new(&[Role::Writer]);
        let paths = &lab.paths[0];
        let config = paths.home.join("config.toml");
        let proposals = Proposals::at(paths.clone());
        proposals.initialize().unwrap();
        proposals.write([1; 16], vec![account("one")]).unwrap();
        let bootstrap = lab.bootstrap(0);
        let scope =
            crate::catalog::Descriptor::application(bootstrap.trust(), "accounts", "proposals", 1)
                .unwrap()
                .scope;
        let directory = crate::catalog::Directory::new(paths, bootstrap.trust());
        let mut missing = Vec::new();
        for (index, scope) in [directory.catalog_scope(), scope].into_iter().enumerate() {
            let data = directory.path(scope).unwrap();
            let retained = paths.home.join(format!("retained-table-{index}"));
            std::fs::rename(&data, &retained).unwrap();
            let before = table_storage_bytes(&paths.dir);
            let observed = accounts(paths, &config, 1000);
            assert_eq!(table_storage_bytes(&paths.dir), before);
            assert!(!data.exists());
            std::fs::rename(&retained, &data).unwrap();
            missing.push((observed.availability, observed.refresh_failed));
        }
        assert!(
            missing.iter().all(|(availability, failed)| {
                *availability == ServiceAvailability::Unknown && *failed
            }),
            "lost enrolled catalog/account storage: {missing:?}"
        );
        let table = directory.path(scope).unwrap().join("table.astream");
        let mut bytes = std::fs::read(&table).unwrap();
        *bytes.last_mut().unwrap() ^= 1;
        std::fs::write(&table, &bytes).unwrap();
        let failed = accounts(paths, &config, 1000);
        assert_eq!(
            failed.availability,
            ServiceAvailability::Unknown,
            "{failed:?}"
        );
        assert!(failed.refresh_failed, "{failed:?}");
    }
}
