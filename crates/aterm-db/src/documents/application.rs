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
    descriptor: fn(&Bootstrap) -> Result<Descriptor>,
    action: impl FnOnce(&Session<'_>, &mut Store, Scope) -> Result<T>,
) -> Result<T> {
    let bootstrap = Bootstrap::required(paths).map_err(crate::table::failure)?;
    let session = Session::open(&bootstrap)?;
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
