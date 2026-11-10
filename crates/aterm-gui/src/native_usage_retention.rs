// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! App-owned custody for quota reads which have already rotated credentials.
//! Capacity is reserved before provider work; retry only settles that response.

use aterm_agent::harness::{account_work, accounts, accounts_pool as pool, cli, provider_read};
use std::path::Path;
use std::sync::{Arc, Mutex};

const CAPACITY: usize = 32;

enum Entry {
    Reserved,
    Retained(pool::NativeUsageRetention),
    Deferred(pool::NativeUsageDeferred),
    Running,
}

#[derive(Clone)]
pub(crate) struct NativeUsageSink {
    entries: Arc<Mutex<Vec<Option<Entry>>>>,
}

struct Reservation {
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
        };
        let worker = sink.clone();
        let admission = app.admission_owner();
        let _driver = std::thread::Builder::new()
            .name("native-usage-storage".into())
            .spawn(move || {
                let mut cursor = pool::NativeUsageDiscoveryCursor::default();
                let mut next_discovery = std::time::Instant::now();
                loop {
                    worker.retry_retained();
                    if std::time::Instant::now() >= next_discovery {
                        cursor = worker.discover_retained(&admission, cursor);
                        next_discovery =
                            std::time::Instant::now() + std::time::Duration::from_secs(60);
                    }
                    // Retainers keep the real counted operation alive after
                    // App drop. Deferred entries own metadata only: abandoning
                    // that locator closes no canonical operation and leaves the
                    // encrypted response/family fence intact. Cold restart
                    // discovery is a separate maintenance doorway.
                    let metadata_only = worker
                        .entries
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .iter()
                        .all(|entry| matches!(entry, None | Some(Entry::Deferred(_))));
                    if metadata_only && Arc::strong_count(&worker.entries) == 1 {
                        break;
                    }
                    // Pacing only, never evidence of retirement or a new
                    // admission. Recovery still runs through app cancellation.
                    std::thread::park_timeout(std::time::Duration::from_secs(1));
                }
            })?;
        Ok(sink)
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
mod tests {
    use super::*;

    #[test]
    fn capacity_is_reserved_before_work_and_released_without_displacing_other_work() {
        let sink = NativeUsageSink {
            entries: Arc::new(Mutex::new(Vec::new())),
        };
        let mut reservations = (0..CAPACITY)
            .map(|_| sink.reserve().expect("bounded admission"))
            .collect::<Vec<_>>();
        assert!(sink.reserve().is_none());
        let original = reservations.pop().unwrap();
        let released = original.index;
        drop(original);
        let replacement = sink.reserve().expect("unused admission returned");
        assert_eq!(replacement.index, released);
        assert!(sink.reserve().is_none());
        drop(replacement);
        drop(reservations);
        assert!(sink.entries.lock().unwrap().iter().all(Option::is_none));
    }

    #[test]
    fn full_or_cancelled_admission_preserves_the_cold_read_allowance() {
        let sink = NativeUsageSink {
            entries: Arc::new(Mutex::new(Vec::new())),
        };
        let lifetime = account_work::Lifetime::default();
        let scope = lifetime.enter().unwrap();
        let reservations = (0..CAPACITY)
            .map(|_| sink.reserve().unwrap())
            .collect::<Vec<_>>();
        assert!(sink.reserve_read(&scope).is_none());
        drop(reservations);
        drop(sink.reserve_read(&scope).unwrap());
        let pause = lifetime.pause().unwrap();
        assert!(sink.reserve_read(&scope).is_none());
        drop(scope);
        assert!(pause.try_wait().is_ok());
    }
}
