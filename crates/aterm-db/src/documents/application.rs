// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::{
    Error, Result,
    catalog::{Descriptor, Directory, State},
    table::{Scope, Session, store::Store},
};
use aterm_secrets::{client::Paths, commons::bootstrap::Bootstrap};

/// Open one application under a fresh membership observation. Callers keep
/// their own schema and descriptor; this helper grants no application authority.
pub(crate) fn with_store<T>(
    paths: &Paths,
    create: bool,
    descriptor: impl FnOnce(&Bootstrap) -> Result<Descriptor>,
    action: impl FnOnce(&Session<'_>, &mut Store, Scope) -> Result<T>,
) -> Result<T> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let (bootstrap, journal) =
        Bootstrap::required_with_journal_until(paths, deadline).map_err(crate::table::failure)?;
    let session = Session::from_admitted_journal(&bootstrap, journal)?;
    let descriptor = descriptor(&bootstrap)?;
    let directory = Directory::new(paths, bootstrap.trust());
    if create {
        directory.publish(&session, &descriptor)?;
    }
    let snapshot = directory.snapshot(&session)?;
    let entry = snapshot
        .entries
        .get(&descriptor.scope.digest())
        .ok_or_else(|| Error::new("application table is not initialized"))?;
    if entry.state != State::Subscribed
        || entry
            .declarations
            .iter()
            .any(|declaration| declaration.descriptor != descriptor)
    {
        return Err(Error::new(
            "application table has an incompatible or unavailable descriptor",
        ));
    }
    let mut store = Store::open(
        &session,
        &directory.path(descriptor.scope)?,
        descriptor.scope,
    )?;
    action(&session, &mut store, descriptor.scope)
}

/// Observe an initialized application without acquiring either journal's writer.
/// The immutable store exposes only its verified custody-witnessed prefix.
pub(crate) fn observe_store<T>(
    paths: &Paths,
    descriptor: impl FnOnce(&Bootstrap) -> Result<Descriptor>,
    action: impl FnOnce(&Session<'_>, &Store, Scope) -> Result<T>,
) -> Result<T> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let bootstrap = Bootstrap::required_until(paths, deadline).map_err(crate::table::failure)?;
    let session = Session::observe(&bootstrap)?;
    let descriptor = descriptor(&bootstrap)?;
    let directory = Directory::new(paths, bootstrap.trust());
    let snapshot = directory.snapshot_existing(&session)?;
    let entry = snapshot
        .entries
        .get(&descriptor.scope.digest())
        .ok_or_else(|| Error::new("application table is not initialized"))?;
    if entry.state != State::Subscribed
        || entry
            .declarations
            .iter()
            .any(|declaration| declaration.descriptor != descriptor)
    {
        return Err(Error::new(
            "application table has an incompatible or unavailable descriptor",
        ));
    }
    let store = Store::observe(
        &session,
        &directory.path(descriptor.scope)?,
        descriptor.scope,
    )?;
    action(&session, &store, descriptor.scope)
}
