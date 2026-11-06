// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Metadata-only service observations for Network State. These reads never
//! activate an account, disclose a credential, or claim fleet convergence.

use aterm_secrets::{
    client::{self, Membership, Paths},
    commons,
};
use aterm_types::network_state::{
    ServiceAvailability, ServiceState, ServiceStates, VaultHttp204Response,
    VaultNetworkMaterialization, VaultNetworkRefresh, VaultNetworkRefreshOutcome,
    VaultNetworkSource,
};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const CURRENT_FOR: Duration = Duration::from_secs(60);

/// One service's facts and the process-local start of the read that produced them.
#[derive(Clone, Debug)]
struct Sample {
    state: ServiceState,
    sampled_at: Instant,
}

impl Sample {
    /// Expire this service, preserving all its last-observed metadata.
    fn current_at(&self, now: Instant, wall_ms: Option<u64>) -> ServiceState {
        let mut service = self.state.clone();
        let current = now
            .checked_duration_since(self.sampled_at)
            .is_some_and(|age| age <= CURRENT_FOR);
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
        service
    }
}

/// Each service's own sample. A slow read of one ages only that one, which expires on its own.
/// Cache readers retain this publication; reading it never renews an age.
#[derive(Clone, Debug)]
pub struct Publication {
    accounts: Sample,
    secrets: Sample,
}

impl Publication {
    /// Both services as sampled together at one instant.
    pub fn new(states: ServiceStates, sampled_at: Instant) -> Self {
        Self {
            accounts: Sample {
                state: states.accounts,
                sampled_at,
            },
            secrets: Sample {
                state: states.secrets,
                sampled_at,
            },
        }
    }

    /// Expire each service independently, preserving all last-observed metadata.
    pub fn current_at(&self, now: Instant, wall_ms: Option<u64>) -> ServiceStates {
        ServiceStates {
            accounts: self.accounts.current_at(now, wall_ms),
            secrets: self.secrets.current_at(now, wall_ms),
        }
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
    clock: impl Fn() -> (Instant, Option<u64>),
    read_accounts: impl FnOnce(u64) -> ServiceState,
    read_secrets: impl FnOnce(u64) -> ServiceState,
) -> Publication {
    fn sampled(
        clock: &impl Fn() -> (Instant, Option<u64>),
        read: impl FnOnce(u64) -> ServiceState,
    ) -> Sample {
        let (sampled_at, wall) = clock();
        Sample {
            state: read(wall.unwrap_or(0)),
            sampled_at,
        }
    }
    Publication {
        accounts: sampled(&clock, read_accounts),
        secrets: sampled(&clock, read_secrets),
    }
}

/// Read service metadata off the window thread, each service's sample starting just before its
/// own potentially slow read. Existing table reads use the ordinary authenticated Commons custody
/// path; no application table is created.
pub fn observe_publication(paths: &Paths, config: &Path) -> Publication {
    sample_with(
        || (Instant::now(), aterm_time::unix_millis()),
        |wall| accounts(paths, config, wall),
        |wall| secrets(paths, wall),
    )
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

pub(crate) fn control_busy(now_ms: u64) -> ServiceState {
    state(
        ServiceAvailability::Unknown,
        now_ms,
        "Commons custody, control history or the account table is busy; waiting to check account access",
    )
}

fn out_of_time(now_ms: u64) -> ServiceState {
    state(
        ServiceAvailability::Unknown,
        now_ms,
        "Ran out of time before account access could be read; this machine may be heavily loaded",
    )
}

fn secrets(paths: &Paths, now_ms: u64) -> ServiceState {
    secrets_reading_refresh(paths, now_ms, client::observe_network_refresh)
}

fn secrets_reading_refresh(
    paths: &Paths,
    now_ms: u64,
    refresh: impl FnOnce(
        &Paths,
        &client::Config,
    ) -> aterm_secrets::Result<Option<client::VaultNetworkRefresh>>,
) -> ServiceState {
    let status = match client::status(paths) {
        Ok(status) => status,
        Err(_) => {
            return failed_observation(now_ms, "Local vault metadata could not be read");
        }
    };
    let Some(config) = &status.config else {
        return secret_metadata(&status.membership, false, status.names.len(), now_ms);
    };
    let mut result = match client::observe_materialization(paths, config, || client::status(paths))
    {
        Ok(client::MaterializationObservation::Ready(held)) => {
            let mut result = secret_metadata(
                &held.inputs.membership,
                true,
                held.inputs.names.len(),
                now_ms,
            );
            result.network_materialization =
                held.snapshot.network_materialization().map(|receipt| {
                    VaultNetworkMaterialization {
                        source: match receipt.source() {
                            client::VaultNetworkSource::ConfiguredUrl => {
                                VaultNetworkSource::ConfiguredUrl
                            }
                            client::VaultNetworkSource::PeerUrl => VaultNetworkSource::PeerUrl,
                        },
                        sequence: receipt.sequence(),
                        sampled_at_ms: receipt.sampled_at_ms(),
                    }
                });
            result
        }
        Ok(client::MaterializationObservation::Blocked(reason)) => secret_blocker(reason, now_ms),
        Err(_) => failed_observation(now_ms, "Local vault cache could not be verified"),
    };
    result.http204_response = client::observe_http204_response(paths, config)
        .ok()
        .flatten()
        .map(|response| VaultHttp204Response {
            source: match response.source() {
                client::VaultNetworkSource::ConfiguredUrl => VaultNetworkSource::ConfiguredUrl,
                client::VaultNetworkSource::PeerUrl => VaultNetworkSource::PeerUrl,
            },
            responded_at_ms: response.responded_at_ms(),
        });
    match refresh(paths, config) {
        Ok(refresh) => {
            result.network_refresh = refresh.map(|refresh| VaultNetworkRefresh {
                completed_at_ms: refresh.completed_at_ms(),
                outcome: match refresh.outcome() {
                    client::VaultNetworkRefreshOutcome::Succeeded => {
                        VaultNetworkRefreshOutcome::Succeeded
                    }
                    client::VaultNetworkRefreshOutcome::Failed => {
                        VaultNetworkRefreshOutcome::Failed
                    }
                },
            });
            result.refresh_failed |= result
                .network_refresh
                .is_some_and(|refresh| refresh.outcome == VaultNetworkRefreshOutcome::Failed);
        }
        Err(_) => {
            result.refresh_failed = true;
            result
                .detail
                .push_str("; network-refresh history could not be read");
        }
    }
    result
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
        Membership::In { seq, .. } => {
            let mut result = if ready && entries > 0 {
                state(
                    ServiceAvailability::Cached,
                    now_ms,
                    &format!(
                        "{entries} manifest entries; local materialization checks passed; provider sign-in and remote currency are not established by this read"
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
            result
        }
    }
}

pub(crate) fn accounts(paths: &Paths, config: &Path, now_ms: u64) -> ServiceState {
    // This observer runs off the window thread. Give an active local batch the short admission
    // time to release custody, and let the read go on until its sample would no longer be current.
    let read_by = Instant::now() + CURRENT_FOR;
    accounts_until(paths, config, now_ms, &crate::admission::deadline, read_by)
}

/// Two bounds for two things. `wait` is asked as each stage that waits for a holder starts, and
/// says when that stage's waits end: the vault and custody locks of this home and, on a successor
/// home, of each predecessor. One answer covers a stage's locks and the reading between them, so
/// reading before the stage costs it nothing and reading inside the stage can spend it. An attempt
/// is made however late it is, so a read says busy only on meeting a holder. Control history and
/// the tables have no wait: they read beside their writers, and a vault writer or a newer witness
/// met there is busy at once.
///
/// `read_by` is when the read says it ran out of time rather than calling the machine's pace
/// contention. It is a gate, not a limit on reading: selection and the session refuse past it, as
/// do a successor home's generation checks at each table it opens. An ordinary home's last gate
/// is the session, so a read past that finishes. Either way a finished read is true however late
/// it finished: a sample starts ageing when it is taken ([`Sample`]), so one read too slowly just
/// shows as no longer current.
pub(crate) fn accounts_until(
    paths: &Paths,
    config: &Path,
    now_ms: u64,
    wait: &dyn Fn() -> Instant,
    read_by: Instant,
) -> ServiceState {
    // A holder or the read's own time stopped it short, so it learned nothing. Any other
    // error is a finding.
    let stopped = |error: &crate::Error| {
        if error.is_busy() {
            Some(control_busy(now_ms))
        } else if Instant::now() >= read_by {
            Some(out_of_time(now_ms))
        } else {
            None
        }
    };
    let selected = match crate::generation::observed_paths_within(paths, read_by, wait) {
        Ok(paths) => paths,
        Err(error) => {
            return stopped(&error).unwrap_or_else(|| {
                failed_observation(now_ms, "Commons writer recovery is pending")
            });
        }
    };
    let paths = &selected;
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
    // Observation must not establish missing current or predecessor custody.
    let bootstrap =
        match commons::bootstrap::Bootstrap::established_with_journal_until(paths, wait()) {
            Ok((bootstrap, _)) => bootstrap,
            Err(error) => {
                return stopped(&crate::table::failure(error)).unwrap_or_else(|| {
                    failed_observation(now_ms, "Commons custody could not be verified")
                });
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
    #[cfg(test)]
    tests::bootstrapped();
    let session = match crate::table::Session::observe_within(&bootstrap, read_by, wait) {
        Ok(session) => session,
        Err(error) => {
            return stopped(&error).unwrap_or_else(|| {
                failed_observation(now_ms, "Account access could not be verified")
            });
        }
    };
    #[cfg(test)]
    tests::observed();
    // The session and table snapshots read beside writers. A newer control
    // history or table witness makes this observation stale; read it afresh.
    // Otherwise changed claims refuse and damaged custody remains a failure.
    let stale = |error: &crate::Error| {
        stopped(error).or_else(|| {
            match crate::table::Session::observe_within(&bootstrap, read_by, wait)
                .and_then(|now| now.observation())
            {
                Ok(current) => session
                    .observation()
                    .is_ok_and(|previous| current != previous)
                    .then(|| control_busy(now_ms)),
                Err(error) => stopped(&error),
            }
        })
    };
    // A retained enrollment claim can outlive its data directory.
    let catalog = match directory.observe_with_config(&session, config) {
        Ok(catalog) => catalog,
        Err(error) => {
            return stale(&error).unwrap_or_else(|| {
                failed_observation(now_ms, "Shared catalog metadata could not be read")
            });
        }
    };
    let blocker = match catalog.entries.get(&descriptor.scope.digest()) {
        None => Some("No retained account catalog declaration; harness rosters are separate"),
        Some(entry) => match entry.state {
            crate::catalog::State::Conflict => {
                Some("Account catalog declarations conflict; automatic access is withheld")
            }
            crate::catalog::State::Excluded => {
                Some("Account catalog is excluded by local sharing policy")
            }
            crate::catalog::State::CreatorLimit => {
                Some("Account catalog declaration exceeds its creator's table limit")
            }
            crate::catalog::State::Capacity => {
                Some("Account catalog is waiting for local table capacity")
            }
            crate::catalog::State::Subscribed => entry
                .declarations
                .iter()
                .any(|d| d.descriptor != descriptor)
                .then_some("Account catalog declaration does not match this aterm's schema"),
        },
    };
    if let Some(blocker) = blocker {
        return state(ServiceAvailability::Unavailable, now_ms, blocker);
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
        Err(error) => {
            return stale(&error).unwrap_or_else(|| {
                failed_observation(now_ms, "Retained account storage could not be opened")
            });
        }
    };
    #[cfg(test)]
    tests::store_observed();
    let snapshot =
        match crate::accounts::proposals::Proposals::snapshot_from_store(&session, &store) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                return stale(&error).unwrap_or_else(|| {
                    failed_observation(now_ms, "Retained account records could not be read")
                });
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
    for (_, cell) in cells
        .range((String::from("accounts"), String::new())..)
        .take_while(|((collection, _), _)| collection == "accounts")
    {
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
pub(crate) mod tests {
    use super::*;

    /// A read's own bound where a test holds it to none: the hang detector.
    fn hang_detector() -> Instant {
        Instant::now() + aterm_spec::wait::MUST_HAPPEN
    }

    fn observe(paths: &Paths, config: &Path, now_ms: u64) -> ServiceStates {
        ServiceStates {
            accounts: accounts(paths, config, now_ms),
            secrets: secrets(paths, now_ms),
        }
    }

    fn sampled_states(wall: u64) -> ServiceStates {
        let service = ServiceState {
            availability: ServiceAvailability::Cached,
            observed_at_ms: wall,
            last_success_ms: Some(wall),
            revision: Some("public revision".into()),
            detail: "retained metadata".into(),
            refresh_failed: true,
            network_materialization: None,
            http204_response: None,
            network_refresh: None,
        };
        ServiceStates {
            accounts: service.clone(),
            secrets: ServiceState {
                network_refresh: Some(VaultNetworkRefresh {
                    completed_at_ms: wall.saturating_sub(3),
                    outcome: VaultNetworkRefreshOutcome::Failed,
                }),
                network_materialization: Some(VaultNetworkMaterialization {
                    source: VaultNetworkSource::PeerUrl,
                    sequence: 66,
                    sampled_at_ms: wall.saturating_sub(1),
                }),
                http204_response: Some(VaultHttp204Response {
                    source: VaultNetworkSource::ConfiguredUrl,
                    responded_at_ms: wall.saturating_sub(2),
                }),
                ..service
            },
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
            assert_eq!(
                expired.secrets.network_materialization,
                states.secrets.network_materialization
            );
            assert_eq!(expired.accounts.network_materialization, None);
            assert_eq!(
                expired.secrets.http204_response,
                states.secrets.http204_response
            );
            assert_eq!(expired.accounts.http204_response, None);
            assert_eq!(
                expired.secrets.network_refresh,
                states.secrets.network_refresh
            );
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
        assert_eq!(publication.accounts.state, states.accounts);
        assert_eq!(publication.secrets.state, states.secrets);
    }

    /// Each sample starts before its own read, so a read past the lease expires its own service
    /// and leaves the next one, read after it, current.
    #[test]
    fn a_slow_accounts_read_expires_accounts_and_leaves_secrets_cached() {
        let at = Instant::now();
        let now = std::cell::Cell::new((at, 100_000));
        let publication = sample_with(
            || {
                let (at, wall) = now.get();
                (at, Some(wall))
            },
            |wall| {
                now.set((at + Duration::from_secs(70), 170_000));
                sampled_states(wall).accounts
            },
            |wall| sampled_states(wall).secrets,
        );
        assert_eq!(publication.accounts.sampled_at, at);
        let (read_at, wall) = now.get();
        assert_eq!(publication.secrets.sampled_at, read_at);
        let current = publication.current_at(read_at, Some(wall));
        assert_eq!(current.accounts.availability, ServiceAvailability::Unknown);
        assert_eq!(current.accounts.observed_at_ms, 100_000);
        assert_eq!(current.secrets.availability, ServiceAvailability::Cached);
        assert_eq!(current.secrets.observed_at_ms, 170_000);
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
        assert_eq!(handle.try_publication().unwrap().accounts.sampled_at, at);
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
            assert_eq!(observation.network_materialization, None);
            assert_eq!(observation.http204_response, None);
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
        assert_eq!(cached.last_success_ms, None);
        assert_eq!(cached.network_materialization, None);
        assert!(!cached.refresh_failed);
        assert_eq!(
            secret_metadata(&membership, true, 0, 15_000).availability,
            ServiceAvailability::Unknown
        );
        assert_eq!(
            secret_metadata(&membership, true, 11, 10_000).last_success_ms,
            None
        );
        let heartbeat = secret_metadata(
            &Membership::In {
                seq: 66,
                heard: u64::MAX,
            },
            true,
            11,
            15_000,
        );
        assert_eq!(heartbeat.last_success_ms, None);
        assert_eq!(heartbeat.network_materialization, None);
    }

    struct VaultFixture {
        root: aterm_tempfile::TempDir,
        server: Option<aterm_secrets::server::Running>,
        listener: std::net::TcpListener,
        paths: Paths,
        url: String,
        /// The server is the test's own on loopback, which answers: this thread's reads of it
        /// wait the hang detector.
        _answering: client::ServersAnswering,
    }

    impl VaultFixture {
        fn new() -> Self {
            use aterm_secrets::{fsx, server};
            let root = aterm_tempfile::tempdir().unwrap();
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let held_listener = listener.try_clone().unwrap();
            let running = server::start(
                listener,
                &server::Options {
                    listen: address,
                    store: root.path().join("store"),
                    advertise: None,
                    pins: Vec::new(),
                    hold: Duration::from_millis(20),
                    keep_awake: false,
                },
            )
            .unwrap();
            let fixture = Self {
                paths: Paths::for_home(&root.path().join("member")),
                url: format!("http://{address}/v1/adapter"),
                root,
                server: Some(running),
                listener: held_listener,
                _answering: client::ServersAnswering::within(aterm_spec::wait::MUST_HAPPEN),
            };
            let import = fixture.root.path().join("entries.secrets");
            fsx::ensure_private_dir(&import).unwrap();
            fsx::write_atomic(&import.join("token"), b"inert-vault-adapter-value\n", 0o600)
                .unwrap();
            client::create(&fixture.paths, &fixture.url, Some(&import), Some("adapter")).unwrap();
            fixture
        }
    }

    impl Drop for VaultFixture {
        fn drop(&mut self) {
            if let Some(server) = self.server.take() {
                server.stop();
            }
        }
    }

    #[test]
    fn failed_network_refresh_keeps_cache_and_receipts_and_recovery_clears_only_failure() {
        use aterm_secrets::server;
        use std::io::{Read as _, Write as _};
        use std::os::fd::AsFd as _;

        let mut fixture = VaultFixture::new();
        let paths = &fixture.paths;
        client::sync_reader_until(paths, Instant::now() + Duration::from_secs(60)).unwrap();
        let before = secrets(paths, 100_000);
        let signed_receipt = before.network_materialization.unwrap();
        assert_eq!(before.availability, ServiceAvailability::Cached);
        assert_eq!(
            before.network_refresh.unwrap().outcome,
            VaultNetworkRefreshOutcome::Succeeded
        );
        fixture.server.take().unwrap().stop();
        // Keep the exact listening socket throughout failure and recovery: the source,
        // configuration and identity stay unchanged, and no port rebind race is involved.
        let listener = fixture.listener.try_clone().unwrap();
        let failed = std::thread::spawn(move || {
            // The stopped server left this shared descriptor nonblocking. It has no
            // other acceptor now; wait for the request before its single accept.
            aterm_uds::ready::until(
                listener.as_fd(),
                aterm_uds::ready::Interest::Read,
                Instant::now() + Duration::from_secs(60),
            )
            .unwrap();
            let (mut stream, _) = aterm_uds::kept_out(|| listener.accept()).unwrap();
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(60)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(60)))
                .unwrap();
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                head.push(byte[0]);
                assert!(head.len() < 16 * 1024);
            }
            let body = b"private-error-body-inert";
            write!(stream, "HTTP/1.1 503 Service Unavailable\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
            stream.write_all(body).unwrap();
        });
        assert!(client::sync_within(paths, Duration::from_secs(60)).is_err());
        failed.join().unwrap();
        let observed = secrets(paths, 110_000);
        assert_eq!(observed.availability, ServiceAvailability::Cached);
        assert!(observed.refresh_failed);
        let refresh = observed.network_refresh.unwrap();
        assert_eq!(refresh.outcome, VaultNetworkRefreshOutcome::Failed);
        assert!(refresh.completed_at_ms > 0);
        assert_eq!(observed.network_materialization, Some(signed_receipt));
        assert_eq!(observed.http204_response, before.http204_response);
        assert_eq!(observed.last_success_ms, None);
        let state_path = paths.dir.join("state.toml");
        let state = std::fs::read(&state_path).unwrap();
        let repeated = secrets(paths, 120_000);
        assert_eq!(repeated.network_refresh, Some(refresh));
        assert_eq!(
            std::fs::read(&state_path).unwrap(),
            state,
            "observation is read-only"
        );
        assert!(!format!("{observed:?}").contains("private-error-body-inert"));
        assert!(
            !String::from_utf8(state)
                .unwrap()
                .contains("private-error-body-inert")
        );

        // Materialization has completed before this reader runs. A writer can win
        // precisely the later history-read race without making the usable cache vanish.
        let retained_bytes = std::fs::read(&state_path).unwrap();
        for discard_busy in [false, true] {
            let raced = secrets_reading_refresh(paths, 120_001, |paths, config| {
                let _writer = aterm_uds::private_file::PrivateLock::take(
                    &paths.dir,
                    ".lock",
                    Duration::from_secs(60),
                    "fixture vault writer is busy",
                )
                .unwrap();
                let actual = client::observe_network_refresh(paths, config);
                assert!(
                    actual.is_err(),
                    "the actual history read meets a held OS lock"
                );
                if discard_busy {
                    // Executable former policy: collapse that actual busy read into absence.
                    actual.or(Ok(None))
                } else {
                    actual
                }
            });
            assert_eq!(raced.availability, ServiceAvailability::Cached);
            assert_eq!(raced.network_materialization, Some(signed_receipt));
            assert_eq!(raced.network_refresh, None);
            assert_eq!(raced.refresh_failed, !discard_busy);
            assert_eq!(
                raced
                    .detail
                    .contains("network-refresh history could not be read"),
                !discard_busy
            );
            assert_eq!(std::fs::read(&state_path).unwrap(), retained_bytes);
        }
        let readable = secrets(paths, 120_002);
        assert_eq!(readable.availability, ServiceAvailability::Cached);
        assert!(readable.refresh_failed);
        assert_eq!(readable.network_refresh, Some(refresh));
        assert_eq!(std::fs::read(&state_path).unwrap(), retained_bytes);

        let address = fixture.listener.local_addr().unwrap();
        fixture.server = Some(
            server::start(
                fixture.listener.try_clone().unwrap(),
                &server::Options {
                    listen: address,
                    store: fixture.root.path().join("store"),
                    advertise: None,
                    pins: Vec::new(),
                    hold: Duration::from_millis(20),
                    keep_awake: false,
                },
            )
            .unwrap(),
        );
        // An actual 204 completes this pass without minting a signed-body receipt.
        client::sync_within(paths, Duration::from_secs(60)).unwrap();
        let recovered = secrets(paths, 130_000);
        assert_eq!(recovered.availability, ServiceAvailability::Cached);
        assert!(!recovered.refresh_failed);
        assert_eq!(
            recovered.network_refresh.unwrap().outcome,
            VaultNetworkRefreshOutcome::Succeeded
        );
        assert_eq!(recovered.network_materialization, Some(signed_receipt));
        assert_eq!(recovered.last_success_ms, None);
        assert!(recovered.http204_response.is_some());
        assert_eq!(before.network_materialization, Some(signed_receipt));
    }

    #[test]
    fn a_guarded_vault_receipt_keeps_its_original_time_and_excludes_changed_local_inputs() {
        use aterm_secrets::{age::Identity, fsx};
        let fixture = VaultFixture::new();
        let root = &fixture.root;
        let paths = fixture.paths.clone();
        let url = fixture.url.clone();
        client::sync_reader_until(&paths, Instant::now() + Duration::from_secs(60)).unwrap();
        let first = secrets(&paths, aterm_time::unix_millis().unwrap());
        assert_eq!(first.availability, ServiceAvailability::Cached);
        assert_eq!(first.last_success_ms, None);
        let receipt = first.network_materialization.unwrap();
        assert_eq!(receipt.source, VaultNetworkSource::ConfiguredUrl);
        assert!(receipt.sampled_at_ms > 0);
        assert_eq!(first.revision, Some(format!("vault {}", receipt.sequence)));
        let later = secrets(&paths, first.observed_at_ms + 20_000);
        assert_eq!(later.network_materialization, Some(receipt));
        assert_eq!(later.last_success_ms, None);
        let recipient = client::read_machine_identity(&paths)
            .unwrap()
            .unwrap()
            .recipient()
            .to_bech32();
        let shown = format!("{later:?}");
        for private in [
            url.as_str(),
            paths.home.to_str().unwrap(),
            recipient.as_str(),
            "inert-vault-adapter-value",
        ] {
            assert!(!shown.contains(private));
        }

        let source_path = paths.dir.join("source.toml");
        let source = std::fs::read_to_string(&source_path).unwrap();
        fsx::write_atomic(
            &source_path,
            source.replace(&url, &format!("{url}-changed")).as_bytes(),
            0o600,
        )
        .unwrap();
        assert_eq!(
            secrets(&paths, first.observed_at_ms).network_materialization,
            None
        );
        fsx::write_atomic(&source_path, source.as_bytes(), 0o600).unwrap();
        assert_eq!(
            secrets(&paths, first.observed_at_ms).network_materialization,
            Some(receipt)
        );
        let mut changed: aterm_toml::Table = aterm_toml::from_str(&source).unwrap();
        changed.insert(
            "host".into(),
            aterm_toml::Value::String("other-host".into()),
        );
        fsx::write_atomic(
            &source_path,
            aterm_toml::to_string(&changed).unwrap().as_bytes(),
            0o600,
        )
        .unwrap();
        let changed_host = secrets(&paths, first.observed_at_ms);
        assert_eq!(changed_host.availability, ServiceAvailability::Cached);
        assert_eq!(changed_host.network_materialization, None);
        fsx::write_atomic(&source_path, source.as_bytes(), 0o600).unwrap();
        assert_eq!(
            secrets(&paths, first.observed_at_ms).network_materialization,
            Some(receipt)
        );

        let identity_path = paths.dir.join("identity");
        let identity = std::fs::read(&identity_path).unwrap();
        let replacement = Identity::from_secret_bytes([117; 32]).to_bech32();
        fsx::write_atomic(&identity_path, replacement.as_slice(), 0o600).unwrap();
        assert_eq!(
            secrets(&paths, first.observed_at_ms).network_materialization,
            None
        );
        fsx::write_atomic(&identity_path, &identity, 0o600).unwrap();
        assert_eq!(
            secrets(&paths, first.observed_at_ms).network_materialization,
            Some(receipt)
        );

        let state_path = paths.dir.join("state.toml");
        let state = std::fs::read_to_string(&state_path).unwrap();
        for (key, value) in [
            (
                "seq",
                aterm_toml::Value::Integer(i64::try_from(receipt.sequence + 1).unwrap()),
            ),
            ("digest", aterm_toml::Value::String("00".repeat(32))),
            ("records_digest", aterm_toml::Value::String("00".repeat(32))),
        ] {
            let mut changed: aterm_toml::Table = aterm_toml::from_str(&state).unwrap();
            changed.insert(key.into(), value);
            fsx::write_atomic(
                &state_path,
                aterm_toml::to_string(&changed).unwrap().as_bytes(),
                0o600,
            )
            .unwrap();
            let changed_version = secrets(&paths, first.observed_at_ms);
            assert_eq!(changed_version.network_materialization, None, "{key}");
            if key == "records_digest" {
                assert_eq!(changed_version.availability, ServiceAvailability::Cached);
            }
            fsx::write_atomic(&state_path, state.as_bytes(), 0o600).unwrap();
            assert_eq!(
                secrets(&paths, first.observed_at_ms).network_materialization,
                Some(receipt)
            );
        }

        let local_source = root.path().join("local.vault");
        let cached = std::fs::read(paths.dir.join("vault")).unwrap();
        fsx::write_atomic(&local_source, &cached, 0o600).unwrap();
        let mut configured: aterm_toml::Table = aterm_toml::from_str(&source).unwrap();
        configured.remove("url");
        configured.insert(
            "path".into(),
            aterm_toml::Value::String(local_source.to_str().unwrap().into()),
        );
        fsx::write_atomic(
            &source_path,
            aterm_toml::to_string(&configured).unwrap().as_bytes(),
            0o600,
        )
        .unwrap();
        client::sync_reader_until(&paths, Instant::now() + Duration::from_secs(60)).unwrap();
        let local = secrets(&paths, first.observed_at_ms + 30_000);
        assert_eq!(local.availability, ServiceAvailability::Cached);
        assert_eq!(local.network_materialization, None);
        assert_eq!(local.last_success_ms, None);
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
    fn an_actual_204_stays_historical_when_local_materialization_is_unavailable() {
        use aterm_secrets::{age::Identity, fsx, materialize};

        let fixture = VaultFixture::new();
        let paths = &fixture.paths;
        let now_ms = aterm_time::unix_millis().unwrap();
        let before = secrets(paths, now_ms);
        assert_eq!(before.availability, ServiceAvailability::Cached);
        assert_eq!(before.network_materialization, None);
        assert_eq!(before.http204_response, None);
        client::sync_within(paths, Duration::from_secs(60)).unwrap();
        let first = secrets(paths, now_ms + 1);
        let response = first
            .http204_response
            .expect("actual server's 204 retained");
        assert_eq!(response.source, VaultNetworkSource::ConfiguredUrl);
        assert!(response.responded_at_ms > 0);
        assert_eq!(first.availability, ServiceAvailability::Cached);
        assert_eq!(first.network_materialization, None);
        assert_eq!(first.last_success_ms, None);
        assert_eq!(
            secrets(paths, now_ms + 20_000).http204_response,
            Some(response)
        );

        let state_path = paths.dir.join("state.toml");
        let state = std::fs::read_to_string(&state_path).unwrap();
        for membership in ["pending", "revoked"] {
            let mut changed: aterm_toml::Table = aterm_toml::from_str(&state).unwrap();
            changed.insert(
                "membership".into(),
                aterm_toml::Value::String(membership.into()),
            );
            fsx::write_atomic(
                &state_path,
                aterm_toml::to_string(&changed).unwrap().as_bytes(),
                0o600,
            )
            .unwrap();
            let unavailable = secrets(paths, now_ms + 20_000);
            assert_eq!(unavailable.availability, ServiceAvailability::Unavailable);
            assert_eq!(unavailable.http204_response, Some(response));
            assert_eq!(unavailable.network_materialization, None);
            assert_eq!(unavailable.last_success_ms, None);
        }
        fsx::write_atomic(&state_path, state.as_bytes(), 0o600).unwrap();
        let token_path = materialize::target_path(&paths.home, "token").unwrap();
        let token = std::fs::read(&token_path).unwrap();
        fsx::write_atomic(&token_path, b"locally-changed", 0o600).unwrap();
        let incomplete = secrets(paths, now_ms + 20_000);
        assert_eq!(incomplete.availability, ServiceAvailability::Unknown);
        assert_eq!(incomplete.http204_response, Some(response));
        assert_eq!(incomplete.network_materialization, None);
        assert_eq!(incomplete.last_success_ms, None);
        fsx::write_atomic(&token_path, &token, 0o600).unwrap();

        let source_path = paths.dir.join("source.toml");
        let source = std::fs::read_to_string(&source_path).unwrap();
        let mut changed: aterm_toml::Table = aterm_toml::from_str(&source).unwrap();
        changed.insert(
            "host".into(),
            aterm_toml::Value::String("other-host".into()),
        );
        fsx::write_atomic(
            &source_path,
            aterm_toml::to_string(&changed).unwrap().as_bytes(),
            0o600,
        )
        .unwrap();
        let changed_host = secrets(paths, now_ms + 20_000);
        assert_eq!(changed_host.availability, ServiceAvailability::Cached);
        assert_eq!(changed_host.http204_response, None);
        fsx::write_atomic(&source_path, source.as_bytes(), 0o600).unwrap();
        let identity_path = paths.dir.join("identity");
        let identity = std::fs::read(&identity_path).unwrap();
        let replacement = Identity::from_secret_bytes([117; 32]).to_bech32();
        fsx::write_atomic(&identity_path, replacement.as_slice(), 0o600).unwrap();
        assert_eq!(secrets(paths, now_ms + 20_000).http204_response, None);
        fsx::write_atomic(&identity_path, &identity, 0o600).unwrap();
        let restored = secrets(paths, now_ms + 20_000);
        assert_eq!(restored.http204_response, Some(response));
        assert_eq!(restored.availability, ServiceAvailability::Cached);
        let recipient = client::read_machine_identity(paths)
            .unwrap()
            .unwrap()
            .recipient()
            .to_bech32();
        let shown = format!("{restored:?}");
        for private in [
            fixture.url.as_str(),
            paths.home.to_str().unwrap(),
            recipient.as_str(),
            "inert-vault-adapter-value",
        ] {
            assert!(!shown.contains(private));
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

    /// A read that runs out of its own time reports that, not a holder and not a verdict: it read
    /// nothing, and nothing holds this home.
    #[test]
    fn a_read_out_of_time_says_so_rather_than_busy_or_unavailable() {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(&root.path().join("not-observed"));
        let lab = crate::table::tests::Lab::new(&[aterm_secrets::commons::roster::Role::Writer]);
        for paths in [&paths, &lab.paths[0]] {
            let config = paths.home.join("config.toml");
            let spent = Instant::now();
            let state = accounts_until(paths, &config, 1000, &hang_detector, spent);
            assert_eq!(state, out_of_time(1000), "{}", paths.home.display());
        }
        assert!(!paths.home.exists());
    }

    /// A slow machine is not a holder. With every wait for a holder already spent as it starts, a
    /// read nothing holds up still finishes, and what it read is the answer: no wall-clock gate on
    /// the way can refuse it, on whatever load.
    #[test]
    fn a_spent_holder_wait_still_reads_what_nobody_holds() {
        use crate::accounts::proposals::Proposals;
        use aterm_secrets::commons::roster::Role;
        let lab = crate::table::tests::Lab::new(&[Role::Writer]);
        let paths = &lab.paths[0];
        let config = paths.home.join("config.toml");
        Proposals::at(paths.clone()).initialize().unwrap();
        let read = accounts_until(paths, &config, 1000, &Instant::now, hang_detector());
        assert_eq!(read.availability, ServiceAvailability::Cached, "{read:?}");
        assert_eq!(read.last_success_ms, Some(1000));
    }

    /// The wait for a holder starts when it is needed, so what the read did before custody cannot
    /// use it up: a read that never reaches custody never starts one.
    #[test]
    fn the_wait_for_a_holder_is_not_taken_before_the_custody_stage() {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(&root.path().join("home"));
        let config = paths.home.join("config.toml");
        let read = accounts_until(
            &paths,
            &config,
            1000,
            &|| panic!("a read that stops before custody took a wait for a holder"),
            hang_detector(),
        );
        assert_eq!(read.availability, ServiceAvailability::Unavailable);
    }

    #[test]
    fn account_observation_never_repairs_missing_current_custody() {
        use crate::accounts::proposals::Proposals;
        use aterm_secrets::commons::roster::Role;

        for name in ["commons.lock", "commons.claim"] {
            let lab = crate::table::tests::Lab::new(&[Role::Writer]);
            let paths = &lab.paths[0];
            let config = paths.home.join("config.toml");
            Proposals::at(paths.clone()).initialize().unwrap();
            assert_eq!(
                accounts(paths, &config, 1000).availability,
                ServiceAvailability::Cached
            );
            let path = paths.dir.join(name);
            std::fs::rename(&path, path.with_extension("preserved")).unwrap();
            let before = crate::tests::files(lab._home.path());

            let observed = accounts(paths, &config, 2000);
            assert!(
                crate::tests::files(lab._home.path()) == before,
                "observing missing {name} changed retained state"
            );
            assert_eq!(observed.availability, ServiceAvailability::Unknown);
            assert!(observed.refresh_failed);
            assert_eq!(observed.last_success_ms, None);
        }
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
        assert_eq!(observation.network_materialization, None);
        assert_eq!(observation.http204_response, None);
        assert!(!observation.detail.contains("private"));
        assert_eq!(proposals.snapshot().unwrap().operations.len(), before);
    }

    #[test]
    fn account_observation_distinguishes_catalog_blockers_without_changing_storage() {
        use crate::{
            accounts::proposals::Proposals,
            catalog::{self, Descriptor, Directory, State},
            table::{Scope, Session, store::Store, tests::Lab},
        };
        use aterm_secrets::{commons::roster::Role, fsx};
        use std::collections::{BTreeMap, BTreeSet};

        let mut reasons = BTreeMap::new();
        for case in [
            "absent",
            "excluded",
            "disabled",
            "conflict",
            "creator",
            "capacity",
            "schema",
            "unretained",
            "usable",
        ] {
            let lab = Lab::new(&[Role::Writer]);
            let paths = &lab.paths[0];
            let config = paths.home.join("config.toml");
            let bootstrap = lab.bootstrap(0);
            let session = Session::open(&bootstrap).unwrap();
            let directory = Directory::new(paths, bootstrap.trust());
            let descriptor = Descriptor::application(
                bootstrap.trust(),
                "accounts",
                "proposals",
                if case == "schema" { 2 } else { 1 },
            )
            .unwrap();
            aterm_journal::create_private_dir_durable(&paths.dir.join("commons/tables")).unwrap();
            let scope = directory.catalog_scope();
            let mut store = Store::join(&session, &directory.path(scope).unwrap(), scope).unwrap();
            if case == "creator" {
                for n in 0..catalog::PER_CREATOR {
                    let earlier = Descriptor::application(
                        bootstrap.trust(),
                        "fixture",
                        &format!("earlier-{n}"),
                        1,
                    )
                    .unwrap();
                    store
                        .commit_catalog(&session, &earlier.encode().unwrap())
                        .unwrap();
                }
            }
            if case != "absent" {
                store
                    .commit_catalog(&session, &descriptor.encode().unwrap())
                    .unwrap();
            }
            if case == "conflict" {
                let mut incompatible = descriptor.clone();
                incompatible.version = 2;
                store
                    .commit_catalog(&session, &incompatible.encode().unwrap())
                    .unwrap();
            }
            drop(store);
            if case == "capacity" {
                for _ in 0..catalog::CATALOG_SLOTS {
                    let scope = Scope::fresh(bootstrap.trust()).unwrap();
                    Store::join(&session, &directory.path(scope).unwrap(), scope).unwrap();
                }
            }
            if case == "excluded" {
                fsx::write_atomic(
                    &config,
                    format!(
                        "[commons]\nexclude_tables = ['{}']\n",
                        aterm_codec::hex::encode_lower(&descriptor.scope.digest())
                    )
                    .as_bytes(),
                    0o600,
                )
                .unwrap();
            } else if case == "disabled" {
                fsx::write_atomic(&config, b"[commons]\nenabled = false\n", 0o600).unwrap();
            }
            let catalog = directory.observe_with_config(&session, &config).unwrap();
            let expected = match case {
                "absent" => None,
                "excluded" | "disabled" => Some(State::Excluded),
                "conflict" => Some(State::Conflict),
                "creator" => Some(State::CreatorLimit),
                "capacity" => Some(State::Capacity),
                _ => Some(State::Subscribed),
            };
            assert_eq!(
                catalog
                    .entries
                    .get(&descriptor.scope.digest())
                    .map(|e| e.state),
                expected,
                "{case} must exercise the actual catalog verdict"
            );
            drop(session);
            drop(bootstrap);
            if case == "usable" {
                Proposals::at(paths.clone()).initialize().unwrap();
            }
            let before = table_storage_bytes(&paths.dir);
            let observed = accounts(paths, &config, 1000);
            assert_eq!(table_storage_bytes(&paths.dir), before, "{case}");
            assert!(!observed.refresh_failed, "{case}");
            if case == "usable" {
                assert_eq!(observed.availability, ServiceAvailability::Cached);
                assert_eq!(observed.last_success_ms, Some(1000));
            } else {
                assert_eq!(
                    observed.availability,
                    ServiceAvailability::Unavailable,
                    "{case}"
                );
                assert_eq!(observed.last_success_ms, None, "{case}");
            }
            reasons.insert(case, observed.detail);
        }
        assert_eq!(reasons["excluded"], reasons["disabled"]);
        assert_eq!(
            reasons.values().collect::<BTreeSet<_>>().len(),
            reasons.len() - 1,
            "distinct catalog blockers must not collapse into one ambiguous reason"
        );
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
        /// Run after custody admission, before the table session revalidates it.
        static BOOTSTRAPPED: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
            const { std::cell::RefCell::new(None) };
        /// Run once [`accounts`] has observed its session.
        static OBSERVED: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
            const { std::cell::RefCell::new(None) };
        /// Run once the account table's witnessed snapshot has opened.
        static STORE_OBSERVED: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
            const { std::cell::RefCell::new(None) };
    }
    pub(super) fn bootstrapped() {
        let hook = BOOTSTRAPPED.with(|hook| hook.borrow_mut().take());
        if let Some(mut hook) = hook {
            hook();
        }
    }
    /// Run `hook` once this thread's next read has admitted its home's custody.
    pub(crate) fn after_custody(hook: impl FnMut() + 'static) {
        BOOTSTRAPPED.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
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
    fn a_vault_writer_during_account_admission_is_busy_then_readable() {
        use crate::accounts::proposals::Proposals;
        use aterm_secrets::commons::roster::Role;
        use std::{cell::RefCell, rc::Rc};
        let lab = crate::table::tests::Lab::new(&[Role::Writer]);
        let paths = lab.paths[0].clone();
        let config = paths.home.join("config.toml");
        Proposals::at(paths.clone()).initialize().unwrap();
        let held = Rc::new(RefCell::new(None));
        let writer = held.clone();
        let locked = paths.clone();
        BOOTSTRAPPED.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                *writer.borrow_mut() = Some(
                    aterm_uds::private_file::PrivateLock::take(
                        &locked.dir,
                        ".lock",
                        Duration::ZERO,
                        "held",
                    )
                    .unwrap(),
                );
            }));
        });
        let busy = accounts(&paths, &config, 1000);
        assert!(BOOTSTRAPPED.with(|hook| hook.borrow().is_none()));
        assert_eq!(busy, control_busy(1000), "{busy:?}");
        drop(held.borrow_mut().take());
        assert_eq!(
            accounts(&paths, &config, 2000).availability,
            ServiceAvailability::Cached
        );
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
        let busy = accounts_until(paths, &config, 1000, &Instant::now, hang_detector());
        assert_eq!(busy, control_busy(1000), "{busy:?}");
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
    fn damaged_custody_during_account_observation_remains_a_failure() {
        use crate::accounts::proposals::Proposals;
        use aterm_secrets::commons::roster::Role;
        let lab = crate::table::tests::Lab::new(&[Role::Writer]);
        let paths = lab.paths[0].clone();
        let config = paths.home.join("config.toml");
        Proposals::at(paths.clone()).initialize().unwrap();
        let pin = paths.dir.join("commons/default.pin");
        let before = std::fs::read(&pin).unwrap();
        let mut corrupted = before.clone();
        corrupted[0] ^= 1;
        let damaged = pin.clone();
        OBSERVED.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                std::fs::write(&damaged, &corrupted).unwrap();
            }));
        });
        let failed = accounts(&paths, &config, 1000);
        std::fs::write(&pin, &before).unwrap();
        assert!(OBSERVED.with(|hook| hook.borrow().is_none()));
        assert_eq!(failed.availability, ServiceAvailability::Unknown);
        assert!(
            failed.refresh_failed,
            "custody damage is not contention: {failed:?}"
        );
        assert_eq!(failed.last_success_ms, None);
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
