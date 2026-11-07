// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared file and source custody checks. Holding these locks is not selection,
//! process, or restart authority; each caller must retain its own admission proof.

use super::{accounts, accounts_sync, cli::Env, codex_app_login, codex_renewal::Identity};
use std::path::Path;

pub(super) struct Custody {
    source: accounts::Row,
    target: accounts::Row,
    source_snapshot: accounts_sync::CodexCompletionSource,
    target_snapshot: accounts_sync::CodexCompletionSource,
    _auth_locks: Vec<codex_app_login::AuthLock>,
}

impl Custody {
    /// Both source documents remain exactly those validated before taking the
    /// credential locks. Call while the enclosing account writer is still held.
    pub(super) fn unchanged(&self, env: &Env) -> Result<bool, &'static str> {
        Ok(accounts_sync::codex_completion_source(env, &self.source)
            .map_err(|_| "source-unreadable")?
            == self.source_snapshot
            && accounts_sync::codex_completion_source(env, &self.target)
                .map_err(|_| "source-unreadable")?
                == self.target_snapshot)
    }
}

/// The caller already holds its account writer and has bound both rows and
/// generations. `None` target identity is only the existing rollback's source-
/// only credential check; it grants no authority to install a destination.
pub(super) fn lock(
    env: &Env,
    source: &accounts::Row,
    target: &accounts::Row,
    old_auth: &Path,
    new_auth: &Path,
    source_identity: &Identity,
    target_identity: Option<&Identity>,
) -> Result<Custody, &'static str> {
    let source_snapshot =
        accounts_sync::codex_completion_source(env, source).map_err(|_| "source-unreadable")?;
    let target_snapshot =
        accounts_sync::codex_completion_source(env, target).map_err(|_| "source-unreadable")?;
    if source_snapshot.held_by.is_some() || target_snapshot.held_by.is_some() {
        return Err("source-held");
    }
    for (row, custody) in [(source, &source_snapshot), (target, &target_snapshot)] {
        if custody.fleet.as_ref().is_some_and(|fleet| {
            fleet.roster.accounts.iter().any(|account| {
                account.label == row.label
                    && (account.vendor != row.vendor
                        || !row
                            .expected_identity_email()
                            .is_some_and(|email| accounts::email_matches(email, &account.email)))
            })
        }) {
            return Err("source-conflict");
        }
    }
    if old_auth == new_auth {
        return Err("same-login");
    }
    let mut paths = vec![old_auth];
    if target_identity.is_some() {
        paths.push(new_auth);
    }
    paths.sort();
    let auth_locks = paths
        .iter()
        .map(|path| {
            codex_app_login::AuthLock::try_take(path)
                .map_err(|_| "auth-lock")?
                .ok_or("auth-busy")
        })
        .collect::<Result<Vec<_>, _>>()?;
    let old_home = old_auth.parent().ok_or("auth-path")?;
    let new_home = new_auth.parent().ok_or("auth-path")?;
    if !crate::ai::facts::codex_file_store(old_home)
        || codex_app_login::renewal_identity(old_home).as_ref() != Some(source_identity)
        || target_identity.is_some_and(|expected| {
            !crate::ai::facts::codex_file_store(new_home)
                || codex_app_login::renewal_identity(new_home).as_ref() != Some(expected)
        })
    {
        return Err("identity-mismatch");
    }
    Ok(Custody {
        source: source.clone(),
        target: target.clone(),
        source_snapshot,
        target_snapshot,
        _auth_locks: auth_locks,
    })
}
