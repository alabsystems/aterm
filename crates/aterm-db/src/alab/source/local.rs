// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Authenticated local absence, separate from an informational missing result.

use super::*;
use crate::table::store::{OwnedTable, ReplicaRevision};

#[cfg(test)]
thread_local! {
    static BEFORE_ENTRY: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

/// Equality token for a complete local catalog with no registry family.
/// This is never a claim about data an offline peer has not delivered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogAbsenceRevision {
    catalog: Digest,
    actor: astream_wire::sync::Actor,
    control: [u8; 64],
    custody: ReplicaRevision,
    inventory: Digest,
}

/// A local absence observed under authenticated catalog and storage custody.
/// Its private constructor grants no provider or shared-budget authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogAbsenceRead {
    revision: CatalogAbsenceRevision,
}

impl CatalogAbsenceRead {
    #[must_use]
    pub fn revision(&self) -> &CatalogAbsenceRevision {
        &self.revision
    }
}

/// Comparison tokens for the two independently validated local source states.
#[derive(Clone, Debug, PartialEq)]
pub enum LocalSourceRevision {
    Present(ProjectionRevision),
    Absent(CatalogAbsenceRevision),
}

/// A complete local native source, with an authenticated absence alternative.
/// Unavailable and disabled sources are errors, never an absent variant.
#[derive(Clone, Debug)]
pub enum LocalSourceRead {
    Present(ProjectionRead),
    Absent(CatalogAbsenceRead),
}

impl LocalSourceRead {
    #[must_use]
    pub fn revision(&self) -> LocalSourceRevision {
        match self {
            Self::Present(read) => LocalSourceRevision::Present(read.revision().clone()),
            Self::Absent(read) => LocalSourceRevision::Absent(read.revision().clone()),
        }
    }
}

impl Registry {
    /// Observe the current complete local source, including catalog-backed absence.
    /// An uninitialized, damaged or inaccessible catalog provides no absence evidence.
    ///
    /// # Errors
    /// Unavailable, incomplete, excluded or unsupported source or local custody.
    pub fn observe_local_source(&self, config: &Path) -> Result<LocalSourceRead> {
        self.with_local_source(config, &self.paths, None, |read, _, validate| {
            (validate.current)()?;
            Ok(read.clone())
        })
    }

    /// Admit bounded local metadata preparation using one native source and vault view.
    /// The caller holds its account writer and compares every candidate through `reader`.
    /// Release all custody before replay, provider actions or network work.
    ///
    /// # Errors
    /// Changed or unavailable source, policy, membership or vault, or callback failure.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "DbLocalSource",
            action = "Case",
            reason = "The environment chooses catalog, storage, policy and callback changes."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbLocalSource",
            action = "Read",
            project = "aterm_db::alab::store::source::local::tests::transition"
        )
    )]
    pub fn with_admitted_local_source<T>(
        &self,
        config: &Path,
        entry_vault: &Paths,
        vault_config: &aterm_secrets::client::Config,
        expected: Option<&LocalSourceRevision>,
        persist: impl FnOnce(&LocalSourceRead, &aterm_secrets::client::EntryReader<'_>) -> Result<T>,
    ) -> Result<T> {
        self.with_local_source(config, entry_vault, expected, |read, look, validate| {
            (validate.current)()?;
            #[cfg(test)]
            if let Some(change) = BEFORE_ENTRY.take() {
                change();
            }
            match look
                .with_entry_reader(entry_vault, vault_config, |reader| {
                    (validate.custody)()?;
                    persist(read, reader)
                })
                .map_err(crate::table::failure)?
            {
                aterm_secrets::client::EntryAdmission::Accepted(result) => result,
                aterm_secrets::client::EntryAdmission::Busy => {
                    Err(Error::busy().context("local source vault"))
                }
                aterm_secrets::client::EntryAdmission::Changed => Err(Error::snapshot_changed()),
                aterm_secrets::client::EntryAdmission::Unavailable => {
                    Err(Error::new("local source vault is unavailable"))
                }
            }
        })
    }

    fn with_local_source<T>(
        &self,
        config: &Path,
        entry_vault: &Paths,
        expected: Option<&LocalSourceRevision>,
        action: impl FnOnce(
            &LocalSourceRead,
            &aterm_secrets::client::VaultLook<'_>,
            &crate::generation::ReadValidation<'_>,
        ) -> Result<T>,
    ) -> Result<T> {
        let deadline = crate::admission::deadline();
        application::with_retained_catalog(&self.paths, entry_vault, deadline, |catalog, look| {
            let context = catalog.context();
            let session = context.session();
            #[cfg(test)]
            if crate::catalog::ABSENCE_MUTANT.get() == 6 {
                session.writer()?;
            }
            let directory = crate::catalog::Directory::new(session.paths(), session.trust());
            let chain = crate::att::REGISTRY.chain();
            let selected = crate::att::current_in(catalog.snapshot(), session.trust(), &chain);
            let selected_scope = selected.descriptor(session.trust())?.scope;
            if catalog
                .snapshot()
                .entries
                .get(&selected_scope.digest())
                .is_some_and(|entry| entry.state == crate::catalog::State::Subscribed)
            {
                return application::with_selected_store(
                    catalog,
                    look,
                    deadline,
                    |session, snapshot| projection_descriptor(config, session, snapshot),
                    |session, store, scope, compiled, custody, look, validate| {
                        let log = Log::read_with(session, store, scope, decode)?;
                        let view = View::read(compiled.table, session, &log);
                        let read = LocalSourceRead::Present(ProjectionRead::read(
                            session, scope, &view, custody,
                        )?);
                        compare(expected, &read)?;
                        let check_custody = || {
                            include_source(config, session.paths(), session.trust(), scope)?;
                            (validate.custody)()
                        };
                        let check_current = || {
                            check_custody()?;
                            (validate.current)()
                        };
                        check_current()?;
                        action(
                            &read,
                            look,
                            &crate::generation::ReadValidation {
                                current: &check_current,
                                custody: &check_custody,
                            },
                        )
                    },
                );
            }
            #[cfg(test)]
            let require_family_absence = crate::catalog::ABSENCE_MUTANT.get() != 5;
            #[cfg(not(test))]
            let require_family_absence = true;
            if require_family_absence
                && catalog.snapshot().entries.values().any(|entry| {
                    entry
                        .declarations
                        .iter()
                        .any(|decl| registry_family(&decl.descriptor))
                })
            {
                return Err(Error::new(
                    "a retained registry generation prevents absence",
                ));
            }
            let mut absent = Vec::with_capacity(chain.len());
            for compiled in chain {
                let scope = compiled.descriptor(session.trust())?.scope;
                include_source(config, session.paths(), session.trust(), scope)?;
                let table = OwnedTable::observe(context, scope)?;
                if !table.is_absent() {
                    return Err(Error::new("undeclared registry custody prevents absence"));
                }
                absent.push(table);
            }
            let inventory = directory.local_absence_inventory(&catalog, &absent)?;
            let read = LocalSourceRead::Absent(CatalogAbsenceRead {
                revision: CatalogAbsenceRevision {
                    catalog: directory.catalog_scope().digest(),
                    actor: session.actor(),
                    control: session.observation()?,
                    custody: catalog.table().revision(),
                    inventory: inventory.fingerprint(),
                },
            });
            compare(expected, &read)?;
            #[cfg(test)]
            let mut catalog = catalog;
            #[cfg(test)]
            if crate::catalog::ABSENCE_MUTANT.get() == 4 {
                catalog.release_guards_for_test();
            }
            let check_custody = || {
                #[cfg(test)]
                let check_catalog = crate::catalog::ABSENCE_MUTANT.get() != 4;
                #[cfg(not(test))]
                let check_catalog = true;
                if check_catalog {
                    catalog.check_custody()?;
                } else {
                    context.check_custody()?;
                }
                for table in &absent {
                    table.check_custody()?;
                }
                inventory.check_storage()?;
                for compiled in crate::att::REGISTRY.chain() {
                    include_source(
                        config,
                        session.paths(),
                        session.trust(),
                        compiled.descriptor(session.trust())?.scope,
                    )?;
                }
                context.check_custody()
            };
            let check_current = || {
                check_custody()?;
                #[cfg(test)]
                if crate::catalog::ABSENCE_MUTANT.get() == 4 {
                    return context.check();
                }
                catalog.check()?;
                if directory
                    .local_absence_inventory(&catalog, &absent)?
                    .fingerprint()
                    != inventory.fingerprint()
                {
                    return Err(Error::snapshot_changed());
                }
                Ok(())
            };
            check_current()?;
            action(
                &read,
                look,
                &crate::generation::ReadValidation {
                    current: &check_current,
                    custody: &check_custody,
                },
            )
        })
    }
}

fn registry_family(descriptor: &Descriptor) -> bool {
    descriptor.application == crate::att::REGISTRY.table.application
        && descriptor.role.split('.').next() == Some(crate::att::REGISTRY.table.role)
}

fn compare(expected: Option<&LocalSourceRevision>, read: &LocalSourceRead) -> Result<()> {
    #[cfg(test)]
    if crate::catalog::ABSENCE_MUTANT.get() == 1 {
        return Ok(());
    }
    if expected.is_some_and(|expected| expected != &read.revision()) {
        return Err(Error::snapshot_changed());
    }
    Ok(())
}

#[cfg(test)]
#[path = "local_tests.rs"]
mod tests;
