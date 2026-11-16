// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! App-owned custody for quota reads which have already rotated credentials.
//! Capacity is reserved before provider work; retry only settles that response.

use aterm_agent::harness::{account_work, accounts, accounts_pool as pool, cli, provider_read};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const CAPACITY: usize = 32;

/// How often the driver retries what it holds while a retry may settle it, as
/// it retried everything before it rested: no event tells that storage became
/// available, that an original operation retired, or that admissions reopened,
/// and a resume in a new admission epoch drops a deferred ticket's backoff, so
/// resting until that backoff ends would let a reopened App wait it out.
const PACE: Duration = Duration::from_secs(1);

/// How often the driver rediscovers durable responses another App or process
/// left unsettled, as it did before it rested: their writer's exit and its
/// lock's release are that process's, and nothing here hears them, and one pass
/// looks at a few of the account store's candidates, its cursor moving on to
/// the next few at the pass after.
const DISCOVER_EVERY: Duration = Duration::from_secs(60);

enum Entry {
    Reserved,
    Retained(pool::NativeUsageRetention),
    Deferred(pool::NativeUsageDeferred),
    Running,
}

impl Entry {
    /// Whether the driver retries this entry on its [`PACE`]: a retained
    /// response, or a deferred ticket a resume may still settle in this App.
    /// A reservation ends with its read (a retained response wakes the driver),
    /// and a parked ticket waits for discovery to replace it.
    fn paces(&self) -> bool {
        match self {
            Self::Retained(_) => true,
            Self::Deferred(deferred) => !deferred.parked(),
            Self::Reserved | Self::Running => false,
        }
    }
}

#[derive(Clone)]
pub(crate) struct NativeUsageSink {
    entries: Arc<Mutex<Vec<Option<Entry>>>>,
    /// Declared after `entries`, so a handle that goes releases its share of
    /// them before its drop wakes the driver to count what is left.
    driver: Driver,
}

/// The recovery driver's thread, once started: woken when a handle goes. The
/// App's last one going ends it, and a reservation holds one, which goes as the
/// read it was made for ends, so a retained response that read committed is
/// retried on its arrival.
#[derive(Clone, Default)]
struct Driver(Arc<OnceLock<std::thread::Thread>>);

impl Driver {
    /// Wake the recovery driver: something it holds may settle now, or a
    /// handle went.
    fn wake(&self) {
        if let Some(driver) = self.0.get() {
            driver.unpark();
        }
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        self.wake();
    }
}

struct Reservation {
    /// A handle on the App's sink: its drop, after the reservation's own, wakes
    /// the driver ([`Driver`]).
    sink: NativeUsageSink,
    index: usize,
    committed: bool,
}

impl NativeUsageSink {
    fn reserve_read(&self, scope: &account_work::Scope) -> Option<Reservation> {
        if scope.cancelled() {
            return None;
        }
        self.reserve()
    }

    /// Establish the recovery driver before any read can obtain capacity.
    /// Failure starts no provider work and leaves no response to abandon.
    pub(crate) fn start(app: &account_work::Lifetime) -> std::io::Result<Self> {
        let sink = Self {
            entries: Arc::new(Mutex::new(Vec::new())),
            driver: Driver::default(),
        };
        let worker = sink.clone();
        let admission = app.admission_owner();
        let driver = std::thread::Builder::new()
            .name("native-usage-storage".into())
            .spawn(move || {
                let mut cursor = pool::NativeUsageDiscoveryCursor::default();
                worker.drive(
                    &mut |worker| {
                        cursor = worker.discover_retained(&admission, std::mem::take(&mut cursor));
                    },
                    &mut std::thread::park_timeout,
                    DISCOVER_EVERY,
                );
            })?;
        let _ = sink.driver.0.set(driver.thread().clone());
        Ok(sink)
    }

    /// The recovery driver: retry what it holds, `discover` every
    /// `discover_every` from its start, and `park` until the next of those is
    /// owed, or until a [`Driver::wake`]. It retries on its [`PACE`] only while it
    /// holds an entry a retry may settle ([`Entry::paces`]); holding none, it
    /// rests until the next discovery. It ends once it holds only deferred
    /// metadata and the App's handles are gone.
    fn drive(
        self,
        discover: &mut dyn FnMut(&Self),
        park: &mut dyn FnMut(Duration),
        discover_every: Duration,
    ) {
        let mut next_discovery = Instant::now();
        loop {
            self.retry_retained();
            if Instant::now() >= next_discovery {
                discover(&self);
                next_discovery = Instant::now() + discover_every;
            }
            // Retainers keep the real counted operation alive after
            // App drop. Deferred entries own metadata only: abandoning
            // that locator closes no canonical operation and leaves the
            // encrypted response/family fence intact. Cold restart
            // discovery is a separate maintenance doorway.
            let (metadata_only, paces) = {
                let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
                (
                    entries
                        .iter()
                        .all(|entry| matches!(entry, None | Some(Entry::Deferred(_)))),
                    entries.iter().flatten().any(Entry::paces),
                )
            };
            if metadata_only && Arc::strong_count(&self.entries) == 1 {
                break;
            }
            // Pacing only, never evidence of retirement or a new
            // admission. Recovery still runs through app cancellation.
            park(if paces {
                PACE
            } else {
                next_discovery.saturating_duration_since(Instant::now())
            });
        }
    }

    fn discover_retained(
        &self,
        admission: &account_work::AdmissionOwner,
        cursor: pool::NativeUsageDiscoveryCursor,
    ) -> pool::NativeUsageDiscoveryCursor {
        let reservations = (0..4).filter_map(|_| self.reserve()).collect::<Vec<_>>();
        if reservations.is_empty() {
            return cursor;
        }
        // Neither App admission nor filesystem/native work runs under entries.
        let Some(scope) = admission.enter() else {
            return cursor;
        };
        let Ok(env) = crate::native_act::codex_account_env() else {
            return cursor;
        };
        let (tickets, cursor) =
            match pool::discover_native_usage_deferred(&env, &scope, cursor, reservations.len()) {
                Ok(found) => found,
                Err(cursor) => return cursor,
            };
        for (ticket, reservation) in tickets.into_iter().zip(reservations) {
            reservation.discover(ticket);
        }
        cursor
    }

    fn reserve(&self) -> Option<Reservation> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let index = match entries.iter().position(Option::is_none) {
            Some(index) => index,
            None if entries.len() < CAPACITY => {
                entries.push(None);
                entries.len() - 1
            }
            None => return None,
        };
        entries[index] = Some(Entry::Reserved);
        Some(Reservation {
            sink: self.clone(),
            index,
            committed: false,
        })
    }

    /// Original counted retainers settle even while paused. Deferred tickets
    /// themselves reserve a genuinely new same-App scope only when admissions
    /// reopen. Neither path can issue another provider request.
    pub(crate) fn retry_retained(&self) {
        for index in 0..CAPACITY {
            let work = {
                let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
                let Some(entry) = entries.get_mut(index) else {
                    break;
                };
                if !matches!(entry, Some(Entry::Retained(_) | Entry::Deferred(_))) {
                    continue;
                }
                entry
                    .replace(Entry::Running)
                    .expect("checked actual pending entry")
            };
            // Both admission and filesystem work happen after releasing the
            // sink mutex. No fresh Lifetime or caller-selected Scope enters.
            let result = match work {
                Entry::Retained(retained) => match retained.retry() {
                    Ok(continuation) => continuation.settle(),
                    Err(retained) => Err(retained),
                },
                Entry::Deferred(deferred) => deferred.resume(),
                _ => unreachable!("pending kind checked under the same lock"),
            };
            let pending = match result {
                Ok(pool::NativeUsageSettlement::Settled) => None,
                Ok(pool::NativeUsageSettlement::Deferred(deferred)) => {
                    Some(Entry::Deferred(deferred))
                }
                Err(retained) => Some(Entry::Retained(retained)),
            };
            let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            entries[index] = pending;
        }
    }
}

impl Reservation {
    fn discover(mut self, ticket: pool::NativeUsageDeferred) {
        let mut entries = self.sink.entries.lock().unwrap_or_else(|e| e.into_inner());
        if entries.iter().any(|entry| {
            matches!(entry,
            Some(Entry::Retained(retained)) if ticket.retained_by(retained))
        }) {
            return;
        }
        if let Some(index) = entries.iter().position(|entry| {
            matches!(entry,
            Some(Entry::Deferred(old)) if ticket.same_journal(old))
        }) {
            // The fresh factory has independently authenticated this journal in
            // the current App. Replacing metadata does not revive the old App.
            entries[index] = Some(Entry::Deferred(ticket));
            return;
        }
        entries[self.index] = Some(Entry::Deferred(ticket));
        self.committed = true;
    }
    fn retain(mut self, retained: pool::NativeUsageRetention) {
        let mut entries = self.sink.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries[self.index] = Some(Entry::Retained(retained));
        self.committed = true;
    }
}

impl Drop for Reservation {
    /// A reservation nothing was committed to gives its capacity back. Either
    /// way its handle ([`Reservation::sink`]) goes after this, and a handle's
    /// drop wakes the driver: what was committed to it (a retained response,
    /// [`Reservation::retain`]) is retried at once, where the driver's rest
    /// could last until the next discovery.
    fn drop(&mut self) {
        if !self.committed {
            let mut entries = self.sink.entries.lock().unwrap_or_else(|e| e.into_inner());
            entries[self.index] = None;
        }
    }
}

/// The scoped product reader. Its source lease must come from the actual
/// constructor when one already holds S; a path cannot manufacture that lease.
pub(crate) struct LiveNativeUsage<'a> {
    pub(crate) env: &'a cli::Env,
    pub(crate) sink: &'a NativeUsageSink,
    pub(crate) scope: &'a account_work::Scope,
    pub(crate) source: Option<pool::NativeUsageSourceLease>,
    pub(crate) read_started: Option<&'a (dyn Fn() + Sync)>,
}

impl pool::UsageReader for LiveNativeUsage<'_> {
    fn read(
        &self,
        cli: accounts::Cli,
        home: &Path,
        expected_email: Option<&str>,
        now: i64,
        use_token: provider_read::Use,
    ) -> aterm_agent::harness::codex_app_login::Reading {
        self.read_observed(cli, home, expected_email, now, use_token)
            .into_reading()
    }

    fn read_observed(
        &self,
        cli: accounts::Cli,
        home: &Path,
        expected_email: Option<&str>,
        now: i64,
        use_token: provider_read::Use,
    ) -> pool::UsageObservation {
        if cli != accounts::Cli::Codex {
            return pool::UsageObservation::unread();
        }
        let Some(reservation) = self.sink.reserve_read(self.scope) else {
            return pool::UsageObservation::unread();
        };
        let first_contact = || {
            if let Some(started) = self.read_started {
                started();
            }
        };
        match pool::read_native_usage_observed(
            pool::NativeUsageRequest {
                env: self.env,
                home,
                expected_email,
                now,
                wait: pool::USAGE_READ_WAIT,
                scope: self.scope,
                use_token,
                source: self.source.clone(),
            },
            &first_contact,
        ) {
            pool::NativeUsageOutcome::Observed(observed) => observed,
            pool::NativeUsageOutcome::Retaining(retained) => {
                reservation.retain(retained);
                pool::UsageObservation::unread()
            }
        }
    }
}

#[cfg(test)]
#[path = "native_usage_retention_tests.rs"]
mod tests;
