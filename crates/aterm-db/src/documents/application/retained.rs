// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Non-mutating custody for a current subscriber's complete local projection.

use super::*;
use crate::table::store::{OwnedTable, ReplicaRevision};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProjectionCustody {
    pub(crate) catalog: ReplicaRevision,
    pub(crate) table: ReplicaRevision,
}

/// A strict catalog projection paired with the physical evidence it came from.
/// Neither the snapshot nor its retained context can be substituted by callers.
pub(crate) struct RetainedCatalog<'c, 'a, 's, 'v> {
    table: OwnedTable<'c, 'a, 's, 'v>,
    snapshot: crate::catalog::Snapshot,
}

impl<'c, 'a, 's, 'v> RetainedCatalog<'c, 'a, 's, 'v> {
    pub(crate) fn context(&self) -> &'c crate::generation::RetainedContext<'a, 's, 'v> {
        self.table.context()
    }

    pub(crate) fn snapshot(&self) -> &crate::catalog::Snapshot {
        &self.snapshot
    }

    pub(crate) fn table(&self) -> &OwnedTable<'c, 'a, 's, 'v> {
        &self.table
    }

    pub(crate) fn check(&self) -> Result<()> {
        self.table.check()
    }

    pub(crate) fn check_custody(&self) -> Result<()> {
        self.table.check_custody()
    }

    #[cfg(test)]
    pub(crate) fn release_guards_for_test(&mut self) {
        self.table.release_guards_for_test();
    }
}

/// Own the complete catalog and its exact retained source context for one call.
/// Missing, incomplete or damaged catalog evidence is unavailable, never empty.
pub(crate) fn with_retained_catalog<T>(
    paths: &Paths,
    entry_vault: &Paths,
    deadline: std::time::Instant,
    action: impl FnOnce(
        RetainedCatalog<'_, '_, '_, '_>,
        &aterm_secrets::client::VaultLook<'_>,
    ) -> Result<T>,
) -> Result<T> {
    crate::generation::with_retained_context(paths, entry_vault, deadline, |context, look| {
        let session = context.session();
        let directory = Directory::new(session.paths(), session.trust());
        let table = OwnedTable::observe(context, directory.catalog_scope())?;
        let snapshot = directory.snapshot_from_owned(&table)?;
        action(RetainedCatalog { table, snapshot }, look)
    })
}

/// Keep both catalog and table replicas excluded through a bounded local
/// projection callback. The callback's validator rechecks all retained guards;
/// call it after projection and immediately before persisting the local intent.
/// Missing required custody refuses rather than creating a source.
pub(crate) fn with_retained_store<T, C>(
    paths: &Paths,
    entry_vault: &Paths,
    deadline: std::time::Instant,
    descriptor: impl FnOnce(&Session<'_>, &crate::catalog::Snapshot) -> Result<(Descriptor, C)>,
    action: impl FnOnce(
        &Session<'_>,
        &Store,
        Scope,
        C,
        ProjectionCustody,
        &aterm_secrets::client::VaultLook<'_>,
        &crate::generation::ReadValidation<'_>,
    ) -> Result<T>,
) -> Result<T> {
    with_retained_catalog(paths, entry_vault, deadline, |catalog, look| {
        with_selected_store(catalog, look, deadline, descriptor, action)
    })
}

/// Select an application from an already owned catalog, keeping both alive.
pub(crate) fn with_selected_store<T, C>(
    catalog: RetainedCatalog<'_, '_, '_, '_>,
    look: &aterm_secrets::client::VaultLook<'_>,
    deadline: std::time::Instant,
    descriptor: impl FnOnce(&Session<'_>, &crate::catalog::Snapshot) -> Result<(Descriptor, C)>,
    action: impl FnOnce(
        &Session<'_>,
        &Store,
        Scope,
        C,
        ProjectionCustody,
        &aterm_secrets::client::VaultLook<'_>,
        &crate::generation::ReadValidation<'_>,
    ) -> Result<T>,
) -> Result<T> {
    let context = catalog.context();
    let session = context.session();
    let catalog_scope = catalog.table.scope();
    #[cfg(test)]
    let mut catalog = catalog;
    let (descriptor, selected) = descriptor(session, catalog.snapshot())?;
    if descriptor.scope == catalog_scope {
        return Err(Error::new(
            "application projection cannot select the catalog itself",
        ));
    }
    let entry = catalog
        .snapshot
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
    let table = OwnedTable::observe(context, descriptor.scope)?;
    let custody = ProjectionCustody {
        catalog: catalog.table.revision(),
        table: table.revision(),
    };
    #[cfg(test)]
    let mut table = table;
    #[cfg(test)]
    if super::DROP_PROJECTION_CUSTODY.get() {
        catalog.table.release_guards_for_test();
        table.release_guards_for_test();
    }
    let check_custody = || {
        #[cfg(test)]
        let check_data = !super::DROP_PROJECTION_CUSTODY.get();
        #[cfg(not(test))]
        let check_data = true;
        if check_data {
            catalog.check_custody()?;
            table.check_custody()?;
        }
        context.check_custody()?;
        pass_current(deadline)
    };
    let revalidate = || {
        check_custody()?;
        #[cfg(test)]
        let check_data = !super::DROP_PROJECTION_CUSTODY.get();
        #[cfg(not(test))]
        let check_data = true;
        if check_data {
            catalog.check()?;
            table.check()?;
        }
        context.check()
    };
    revalidate()?;
    action(
        session,
        table.require_store()?,
        descriptor.scope,
        selected,
        custody,
        look,
        &crate::generation::ReadValidation {
            current: &revalidate,
            custody: &check_custody,
        },
    )
}
