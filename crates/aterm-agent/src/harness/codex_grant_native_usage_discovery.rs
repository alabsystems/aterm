// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A fresh App may rediscover durable responses only through current canonical
//! owner authentication. The cursor is ordering metadata, never admission.
use super::*;

const CANDIDATES_PER_PASS: usize = 4;

#[derive(Default)]
pub struct NativeUsageDiscoveryCursor {
    store: Option<PathBuf>,
    after: Option<String>,
}
impl std::fmt::Debug for NativeUsageDiscoveryCursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeUsageDiscoveryCursor(ordering-only)")
    }
}

/// The caller must already own a genuine current App operation. No locator
/// revives an old App: each returned ticket belongs to this discovery operation
/// and cannot resume until that actual operation retires. This reads and
/// authenticates existing state only; it never sends, materializes, or repairs.
/// Errors return the cursor so contention cannot erase scan progress.
pub fn discover_native_usage_deferred(
    env: &Env,
    scope: &account_work::Scope,
    mut cursor: NativeUsageDiscoveryCursor,
    capacity: usize,
) -> Result<(Vec<NativeUsageDeferred>, NativeUsageDiscoveryCursor), NativeUsageDiscoveryCursor> {
    if scope.cancelled() || capacity == 0 {
        return Err(cursor);
    }
    let Some(store) = env.accounts_store_for(accounts::Cli::Codex) else {
        return Err(cursor);
    };
    if cursor.store.as_ref() != Some(&store.dir) {
        cursor.store = Some(store.dir.clone());
        cursor.after = None;
    }
    let Ok((logins, None)) = store.load_logins(env.now) else {
        return Err(cursor);
    };
    let candidates = logins
        .enrollment
        .global_owner_candidates_after(cursor.after.as_deref(), capacity.min(CANDIDATES_PER_PASS));
    if candidates.is_empty() {
        cursor.after = None;
        return Ok((Vec::new(), cursor));
    }
    let Some(home) = env.home.as_deref() else {
        return Err(cursor);
    };
    // Existing-only acquisition refuses missing/replaced private namespaces.
    // It precedes exchange/account/owner/Auth claims in the normal hierarchy.
    let Ok(source) = codex_source_gate::SourceWorkLease::for_existing_home(home) else {
        return Err(cursor);
    };
    let Ok(identity) = source.identity() else {
        return Err(cursor);
    };
    let source = Arc::new(source);
    let mut tickets = Vec::new();
    for binding in candidates {
        if scope.cancelled() {
            break;
        }
        // Advance even when this candidate is locked or policy-ineligible.
        // Every later pass still authenticates its own candidate from scratch.
        cursor.after = Some(binding.request.id.clone());
        let authenticated =
            transaction::with_refresh_work_scoped(env, scope, Some(Arc::clone(&source)), |work| {
                let result = super::super::super::with_maintenance_owner(
                    env,
                    &binding,
                    scope,
                    |mut grant| {
                        let witness = grant.anchor().read()?.ok_or("owner witness absent")?;
                        let claimed = Claimed::open(env, &binding, false)?
                            .ok_or("owner response journal absent")?;
                        let current = claimed.view(&witness)?;
                        super::super::super::catalogue::require_participant(
                            env,
                            &binding,
                            &current.identity,
                        )?;
                        let pending = claimed
                            .journal
                            .as_ref()
                            .and_then(|journal| journal.pending.as_ref())
                            .ok_or("owner response absent")?;
                        if pending.response.is_none()
                            || !matches!(
                                pending.phase,
                                Phase::Responded
                                    | Phase::Publishing
                                    | Phase::Rejected
                                    | Phase::Quarantined
                            )
                        {
                            return Err("owner response is not durably received".into());
                        }
                        claimed.store.unchanged().map_err(str::to_owned)?;
                        Ok(())
                    },
                );
                work.current().map_err(|_| OwnerRefreshError::Unavailable)?;
                result.map_err(|_| OwnerRefreshError::Unavailable)
            });
        if authenticated.is_ok() && !scope.cancelled() && identity.current() {
            tickets.push(NativeUsageDeferred(Box::new(DeferredState {
                env: env.clone(),
                binding,
                owner: scope.maintenance_owner(),
                source: Some(identity.clone()),
                epoch: Some(scope.maintenance_epoch()),
                not_before: None,
                failures: 0,
                parked_namespace: false,
            })));
        }
    }
    if scope.cancelled() || !identity.current() {
        return Err(cursor);
    }
    Ok((tickets, cursor))
}
