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
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle, Thread},
    time::{Duration, Instant},
};

const CURRENT_FOR: Duration = Duration::from_secs(60);

/// How long the worker rests after each sample, however often it is read meanwhile.
const EVERY: Duration = Duration::from_secs(10);

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

/// The publication, and what tells its worker that a reader wants another sample.
struct Shared {
    publication: Mutex<Publication>,
    /// A reader took the publication since the worker last began a sample.
    asked: AtomicBool,
    /// The worker a read wakes: set before its runtime hands out a handle, never for a handle
    /// no worker serves.
    worker: OnceLock<Thread>,
    stopped: AtomicBool,
}

impl Shared {
    fn replace(&self, publication: Publication) {
        *self
            .publication
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = publication;
    }
}

/// A read-only publication handle. Busy reads provide no new sample.
#[derive(Clone)]
pub struct ObservationHandle(Arc<Shared>);

impl Default for ObservationHandle {
    fn default() -> Self {
        Self(Arc::new(Shared {
            publication: Mutex::new(Publication::new(ServiceStates::default(), Instant::now())),
            asked: AtomicBool::new(false),
            worker: OnceLock::new(),
            stopped: AtomicBool::new(false),
        }))
    }
}

impl ObservationHandle {
    /// The last publication, or none while its worker is replacing it. Each read, busy or not,
    /// asks for the next sample: the worker samples only after a read, so services nobody reads
    /// are not observed and their last sample just ages.
    pub fn try_publication(&self) -> Option<Publication> {
        self.ask();
        match self.0.publication.try_lock() {
            Ok(publication) => Some(publication.clone()),
            Err(std::sync::TryLockError::Poisoned(error)) => Some(error.into_inner().clone()),
            Err(std::sync::TryLockError::WouldBlock) => None,
        }
    }

    /// Ask for a sample, waking the worker only for the first ask since its last sample began.
    /// The worker clears `asked` with a swap too, so of an ask and its check one sees the other.
    fn ask(&self) {
        if !self.0.asked.swap(true, Ordering::AcqRel)
            && let Some(worker) = self.0.worker.get()
        {
            worker.unpark();
        }
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
        MaterializationBlocker::WriterBusy => (
            ServiceAvailability::Unknown,
            "A vault pass on this machine holds its state lock; awaiting a new observation",
        ),
        MaterializationBlocker::LockMissing => (
            ServiceAvailability::Unknown,
            "Vault state cannot be observed: its observation lock is missing",
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
        // No path here: a blocker's detail names none (`aterm secrets status` names the folder).
        MaterializationBlocker::Dropped => (
            ServiceAvailability::Unavailable,
            "Vault membership was dropped: no seat, and a rotation left this Mac out with no Remove; set the vault folder aside (aterm secrets status names it) to come back as a new Mac",
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
    // Name the home this read opens; a successor home's predecessors are observed once, after its
    // own custody is opened below.
    let selected = match crate::generation::observed_generation_until(paths, read_by) {
        Ok(paths) => paths,
        Err(error) => {
            return stopped(&error).unwrap_or_else(|| {
                failed_observation(now_ms, "Commons writer recovery is pending")
            });
        }
    };
    let paths = &selected;
    // Sampled each 10 s while read, whatever it finds: unchanged vault files keep the last
    // observation.
    match commons::observe_settled(paths) {
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
    let (bootstrap, journal) =
        match commons::bootstrap::Bootstrap::established_with_journal_until(paths, wait()) {
            Ok(established) => established,
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
    // A successor home's predecessors, observed once, after every wait for this home's custody;
    // the session keeps that observation for its checks.
    let selection = match crate::generation::observe_generation(bootstrap.paths(), read_by, wait) {
        Ok(selection) => selection,
        Err(error) => {
            return stopped(&error).unwrap_or_else(|| {
                failed_observation(now_ms, "Commons writer recovery is pending")
            });
        }
    };
    let session =
        match crate::table::Session::observe_selected(&bootstrap, journal, selection, read_by) {
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
        stopped(error).or_else(|| match session.observation_afresh() {
            Ok(current) => session
                .observation()
                .is_ok_and(|previous| current != previous)
                .then(|| control_busy(now_ms)),
            Err(error) => stopped(&error),
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

/// The worker's body. It waits for a read, samples, publishes, then rests a whole `every`
/// whatever is read meanwhile (a read during a sample or a rest asks for the sample after it,
/// not an earlier one), and goes round again: while something reads at least once a rest, it
/// samples once each `every`; once nothing does, it parks until a read or a stop. `wait(None)`
/// parks until either wakes it, `wait(Some(until))` until `until` at the latest; either may
/// return early.
fn serve(
    shared: &Shared,
    every: Duration,
    mut observe: impl FnMut() -> Publication,
    mut wait: impl FnMut(Option<Instant>),
) {
    loop {
        while !shared.stopped.load(Ordering::Acquire) && !shared.asked.swap(false, Ordering::AcqRel)
        {
            wait(None);
        }
        if shared.stopped.load(Ordering::Acquire) {
            return;
        }
        shared.replace(observe());
        let until = Instant::now() + every;
        while !shared.stopped.load(Ordering::Acquire) && Instant::now() < until {
            wait(Some(until));
        }
    }
}

/// The worker thread's wait: parked until a read or a stop unparks it, or until `until`.
fn park_until(until: Option<Instant>) {
    match until {
        None => thread::park(),
        Some(until) => thread::park_timeout(until.saturating_duration_since(Instant::now())),
    }
}

/// A process-owned worker; window reads only its immutable metadata snapshots. It observes the
/// services only for a reader ([`ObservationHandle::try_publication`]), at most once each
/// [`EVERY`].
pub struct Runtime {
    shared: ObservationHandle,
    worker: Option<JoinHandle<()>>,
}

impl Runtime {
    pub fn start(paths: Paths, config: PathBuf) -> crate::Result<Self> {
        Self::start_with(
            move || observe_publication(&paths, &config),
            EVERY,
            park_until,
        )
    }

    /// The runtime over `observe`, resting `every`, its worker waiting with `wait` ([`serve`]):
    /// [`park_until`], or a law's wrapper of it.
    fn start_with(
        observe: impl FnMut() -> Publication + Send + 'static,
        every: Duration,
        wait: impl FnMut(Option<Instant>) + Send + 'static,
    ) -> crate::Result<Self> {
        let shared = ObservationHandle::default();
        let serving = shared.clone();
        let worker = thread::Builder::new()
            .name("network-services".into())
            .spawn(move || serve(&serving.0, every, observe, wait))?;
        // Before any handle but the worker's exists, so no read can miss the worker: one before
        // its first park leaves `asked` for it, and one after unparks it.
        let _ = shared.0.worker.set(worker.thread().clone());
        Ok(Self {
            shared,
            worker: Some(worker),
        })
    }

    pub fn observation(&self) -> ObservationHandle {
        self.shared.clone()
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.shared.0.stopped.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
#[path = "service_state_tests.rs"]
pub(crate) mod tests;
