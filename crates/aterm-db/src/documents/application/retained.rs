// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Non-mutating custody for a current subscriber's primary projection.

use super::*;
use crate::table::store::{Contributor, Region};

/// Keep primary catalog and table writers excluded through a bounded local
/// projection callback. The callback's validator rechecks all retained guards;
/// call it after projection and immediately before persisting the local intent.
/// Missing or recovery-only custody refuses rather than creating a source.
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
        &aterm_secrets::client::VaultLook<'_>,
        &crate::generation::ReadValidation<'_>,
    ) -> Result<T>,
) -> Result<T> {
    crate::generation::with_retained_session(
        paths,
        entry_vault,
        deadline,
        |session, look, validate| {
            let directory = Directory::new(session.paths(), session.trust());
            let catalog_scope = directory.catalog_scope();
            let catalog = Contributor::observe(
                session,
                &directory.path(catalog_scope)?,
                catalog_scope,
                Region::Primary,
            )?;
            #[cfg(test)]
            let mut catalog = catalog;
            let snapshot = directory.snapshot_from_primary(session, catalog.require_store()?)?;
            let (descriptor, context) = descriptor(session, &snapshot)?;
            if descriptor.scope == catalog_scope {
                return Err(Error::new(
                    "application projection cannot select the catalog itself",
                ));
            }
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
            let table = Contributor::observe(
                session,
                &directory.path(descriptor.scope)?,
                descriptor.scope,
                Region::Primary,
            )?;
            #[cfg(test)]
            let mut table = table;
            #[cfg(test)]
            if super::DROP_PROJECTION_CUSTODY.get() {
                catalog.release_guard_for_test();
                table.release_guard_for_test();
            }
            let check_custody = || {
                catalog.check_custody()?;
                table.check_custody()?;
                (validate.custody)()?;
                pass_current(deadline)
            };
            let revalidate = || {
                check_custody()?;
                catalog.check(session)?;
                table.check(session)?;
                (validate.current)()
            };
            revalidate()?;
            action(
                session,
                table.require_store()?,
                descriptor.scope,
                context,
                look,
                &crate::generation::ReadValidation {
                    current: &revalidate,
                    custody: &check_custody,
                },
            )
        },
    )
}
