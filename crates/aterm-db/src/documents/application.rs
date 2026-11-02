// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::admission::deadline as admission_deadline;
use crate::{
    Error, Result,
    catalog::{Descriptor, Directory, State},
    database::Command,
    table::{
        Scope, Session,
        store::{Receipt, Store},
    },
};
use aterm_secrets::{client::Paths, commons::bootstrap::Bootstrap};

/// Recognize only a canonical value-free join, independently of the
/// application's value or authority schema.
pub(crate) fn decode_merge(payload: &[u8]) -> Option<Command> {
    let command = Command::decode(payload).ok()?;
    if matches!(command, Command::Merge { .. }) && command.encode().ok()?.as_slice() == payload {
        Some(command)
    } else {
        None
    }
}

/// Append a typed application record, using value-free commands to join wide
/// causal frontiers. The shared application log recognizes these commands
/// independently of each application's business schema.
/// Callers validate payloads and retries first. A caller with record-count
/// budgets must also admit the additional retained merge records.
pub(crate) fn append(session: &Session<'_>, store: &mut Store, payload: &[u8]) -> Result<Receipt> {
    store.commit_catalog_with_merges(session, payload, || {
        Command::Merge {
            request: crate::crypto::random()?,
        }
        .encode()
    })
}

/// Open one application under a fresh membership observation. Callers keep
/// their own schema and descriptor; this helper grants no application authority.
pub(crate) fn with_store<T>(
    paths: &Paths,
    create: bool,
    descriptor: impl FnOnce(&Bootstrap) -> Result<Descriptor>,
    action: impl FnOnce(&Session<'_>, &mut Store, Scope) -> Result<T>,
) -> Result<T> {
    with_store_until(paths, create, descriptor, admission_deadline(), action)
}

/// Preserve one caller-owned deadline through custody and every table writer
/// acquired by this session.
pub(crate) fn with_store_until<T>(
    paths: &Paths,
    create: bool,
    descriptor: impl FnOnce(&Bootstrap) -> Result<Descriptor>,
    deadline: std::time::Instant,
    action: impl FnOnce(&Session<'_>, &mut Store, Scope) -> Result<T>,
) -> Result<T> {
    with_store_guarded_until(
        paths,
        create,
        descriptor,
        deadline,
        |_, _, _| Ok(()),
        action,
    )
}

/// Check application-specific independent custody before publication can enroll
/// storage. The caller shares the admitted control writer with this guard.
pub(crate) fn with_store_guarded_until<T>(
    paths: &Paths,
    create: bool,
    descriptor: impl FnOnce(&Bootstrap) -> Result<Descriptor>,
    deadline: std::time::Instant,
    before_publication: impl FnOnce(&Session<'_>, &Directory, &Descriptor) -> Result<()>,
    action: impl FnOnce(&Session<'_>, &mut Store, Scope) -> Result<T>,
) -> Result<T> {
    pass_current(deadline)?;
    let paths = crate::application_paths_until(paths, deadline)?;
    let paths = &paths;
    let (bootstrap, journal) =
        Bootstrap::required_with_journal_until(paths, deadline).map_err(crate::table::failure)?;
    let session = Session::from_admitted_journal_until(&bootstrap, journal, Some(deadline))?;
    pass_current(deadline)?;
    let descriptor = descriptor(&bootstrap)?;
    let directory = Directory::new(paths, bootstrap.trust());
    before_publication(&session, &directory, &descriptor)?;
    if create {
        directory.publish(&session, &descriptor)?;
    }
    let snapshot = if create {
        directory.snapshot(&session)?
    } else {
        directory.snapshot_existing(&session)?
    };
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
    pass_current(deadline)?;
    let mut store = Store::open(
        &session,
        &directory.path(descriptor.scope)?,
        descriptor.scope,
    )?;
    pass_current(deadline)?;
    action(&session, &mut store, descriptor.scope)
}

/// Observe an initialized application without initializing custody or acquiring
/// either journal's writer.
/// The immutable store exposes only its verified custody-witnessed prefix.
pub(crate) fn observe_store<T>(
    paths: &Paths,
    descriptor: impl FnOnce(&Bootstrap) -> Result<Descriptor>,
    action: impl FnOnce(&Session<'_>, &Store, Scope) -> Result<T>,
) -> Result<T> {
    observe_store_until(paths, descriptor, admission_deadline(), action)
}

pub(crate) fn observe_store_until<T>(
    paths: &Paths,
    descriptor: impl FnOnce(&Bootstrap) -> Result<Descriptor>,
    deadline: std::time::Instant,
    action: impl FnOnce(&Session<'_>, &Store, Scope) -> Result<T>,
) -> Result<T> {
    pass_current(deadline)?;
    let paths = crate::generation::observed_paths_until(paths, deadline)?;
    let paths = &paths;
    let (bootstrap, _) = Bootstrap::established_with_journal_until(paths, deadline)
        .map_err(crate::table::failure)?;
    let session = Session::observe_until(&bootstrap, deadline)?;
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
    let store = directory.observe_table(&session, descriptor.scope)?;
    pass_current(deadline)?;
    action(&session, &store, descriptor.scope)
}

fn pass_current(deadline: std::time::Instant) -> Result<()> {
    if std::time::Instant::now() >= deadline {
        Err(Error::new("application pass deadline expired"))
    } else {
        Ok(())
    }
}
