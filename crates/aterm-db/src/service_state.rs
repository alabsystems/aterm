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
    time::Duration,
};

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

fn secrets(paths: &Paths, now_ms: u64) -> ServiceState {
    let status = match client::status(paths) {
        Ok(status) => status,
        Err(_) => {
            return state(
                ServiceAvailability::Unknown,
                now_ms,
                "Local vault metadata could not be read",
            );
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
        Err(_) => state(
            ServiceAvailability::Unknown,
            now_ms,
            "Local vault cache could not be verified",
        ),
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
                        "{entries} authorized local entries; remote currency is not established by this read"
                    ),
                )
            } else if ready {
                state(
                    ServiceAvailability::Unknown,
                    now_ms,
                    "Vault is available but supplies no local entries",
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
            return state(
                ServiceAvailability::Unknown,
                now_ms,
                "Commons access could not be verified",
            );
        }
        Ok(commons::Observation::Visible(_)) => {}
    }
    // Do not let observing an uninitialized service create its catalog or table.
    match paths.dir.join("commons/tables").try_exists() {
        Ok(false) => {
            return state(
                ServiceAvailability::Unavailable,
                now_ms,
                "No shared account catalog is retained here",
            );
        }
        Err(_) => {
            return state(
                ServiceAvailability::Unknown,
                now_ms,
                "Retained catalog storage could not be inspected",
            );
        }
        Ok(true) => {}
    }
    let bootstrap = match commons::bootstrap::Bootstrap::cached(paths) {
        Ok(Some(bootstrap)) => bootstrap,
        _ => {
            return state(
                ServiceAvailability::Unknown,
                now_ms,
                "Commons custody could not be verified",
            );
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
            return state(
                ServiceAvailability::Unknown,
                now_ms,
                "Account catalog identity is unavailable",
            );
        }
    };
    for scope in [directory.catalog_scope(), descriptor.scope] {
        match directory
            .path(scope)
            .and_then(|path| path.try_exists().map_err(Into::into))
        {
            Ok(false) => {
                return state(
                    ServiceAvailability::Unavailable,
                    now_ms,
                    "Shared account catalog has not been received or initialized",
                );
            }
            Err(_) => {
                return state(
                    ServiceAvailability::Unknown,
                    now_ms,
                    "Retained account storage could not be inspected",
                );
            }
            Ok(true) => {}
        }
    }
    let session = match crate::table::Session::open(&bootstrap) {
        Ok(session) => session,
        Err(_) => {
            return state(
                ServiceAvailability::Unavailable,
                now_ms,
                "Commons membership does not currently allow account access",
            );
        }
    };
    let catalog = match directory.snapshot_existing_with_config(&session, config) {
        Ok(catalog) => catalog,
        Err(_) => {
            return state(
                ServiceAvailability::Unknown,
                now_ms,
                "Shared catalog metadata could not be read",
            );
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
    let store = match directory
        .path(descriptor.scope)
        .and_then(|path| crate::table::store::Store::open(&session, &path, descriptor.scope))
    {
        Ok(store) => store,
        Err(_) => {
            return state(
                ServiceAvailability::Unknown,
                now_ms,
                "Retained account storage could not be opened",
            );
        }
    };
    let snapshot =
        match crate::accounts::proposals::Proposals::snapshot_from_store(&session, &store) {
            Ok(snapshot) => snapshot,
            Err(_) => {
                return state(
                    ServiceAvailability::Unknown,
                    now_ms,
                    "Retained account records could not be read",
                );
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
    shared: Arc<Mutex<ServiceStates>>,
    stop: Arc<Stop>,
    worker: Option<JoinHandle<()>>,
}

impl Runtime {
    pub fn start(paths: Paths, config: PathBuf) -> crate::Result<Self> {
        let shared = Arc::new(Mutex::new(ServiceStates::default()));
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
                    let now = aterm_time::unix_secs().unwrap_or(0).saturating_mul(1000);
                    *publish.lock().unwrap_or_else(|p| p.into_inner()) =
                        observe(&paths, &config, now);
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

    pub fn observation(&self) -> Arc<Mutex<ServiceStates>> {
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
}
